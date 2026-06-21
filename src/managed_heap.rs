//! Модуль управляемой кучи (Managed Heap).
//!
//! `ManagedHeap` представляет собой единый непрерывный массив байт, в котором выделяются
//! спаны (`MSpan`) под объекты управляемого рантайма. Он отвечает только за выделение
//! новых регионов памяти (спанов) и низкоуровневое чтение/запись слотов по их `ObjectId`.
//! Непосредственным размещением объектов внутри спанов занимаются `MCache` и `MCentral`.
use std::cell::UnsafeCell;
use std::sync::{Arc, Mutex};
use crate::datastruct::{AllocError, ManagedObject, ObjectId, TypeDescriptor, OBJ_HEADER_SIZE, PAGE_SIZE};
use crate::mspan::{MSpan, MSpanSizeClass};

/// Управляемая куча — центральное хранилище всех объектов рантайма.
///
/// ## Потокобезопасность
///
/// Структура реализует `Sync` и `Send` вручную (см. unsafe impl блоки).
/// Это разрешено, потому что:
/// - Чтение/запись данных происходит через атомарные операции в спанах (`MSpan`)
///   и синхронизируется на уровне аллокатора.
/// - Метаданные (`HeapMeta`) защищены собственным `Mutex`.
/// - Само тело кучи `memory` обёрнуто в `UnsafeCell`, чтобы Rust не накладывал
///   ограничения на разделяемый доступ. Потенциальные гонки предотвращаются
///   логикой аллокатора (один слот никогда не пишется параллельно).
pub struct ManagedHeap {
    memory: UnsafeCell<Vec<u8>>,
    memory_size: usize,
    meta: Mutex<HeapMeta>,
}

pub struct HeapMeta {
    /// Абсолютное смещение в memory откуда мы будем доставать следующую страницу для нового MSpan
    pub next_page_offset: usize,
    /// Карта страниц: индекс — номер страницы (смещение / 4096), значение —
    /// ссылка на `MSpan`, который управляет этой страницей. Одна страница может
    /// принадлежать только одному спану, но спан может занимать несколько страниц.
    pub page_map: Vec<Option<Arc<MSpan>>>,
}

// SAFETY: Доступ к `memory` синхронизирован логически на уровне спанов и Mutex.
//         Одновременное чтение разных слотов безопасно, запись — защищена аллокатором.
unsafe impl Sync for ManagedHeap {}
unsafe impl Send for ManagedHeap {}

impl ManagedHeap {
    pub fn new(heap_size: usize) -> Self {
        // Искусственно «откусываем» первые 4096 байт, чтобы смещение 0
        // никогда не досталось живому объекту и гарантированно означало None
        // Гарантируем, что куча как минимум больше одной страницы
        assert!(heap_size > PAGE_SIZE, "Размер кучи должен быть больше 4096 байт");

        Self {
            memory: UnsafeCell::new(vec![0u8; heap_size]),
            memory_size: heap_size,
            meta: Mutex::new(HeapMeta {
                // Начинаем строго с 4096! Первая страница (0..4096) остается пустой.
                next_page_offset: PAGE_SIZE,
                page_map: vec![None; (heap_size / PAGE_SIZE) + 1],
            }),
        }
    }

    /// Выделяет новый спан заданного класса размеров, «откусывая» необходимый
    /// непрерывный участок кучи.
    ///
    /// Возвращает `Arc<MSpan>`, который автоматически регистрируется в карте страниц,
    /// и на который затем могут ссылаться `MCache` и `MCentral`.
    ///
    /// # Ошибки
    /// `AllocError::OutOfMemory` — если в куче недостаточно свободного места.
    pub fn grow_span(&self, size_class: MSpanSizeClass) -> Result<Arc<MSpan>, AllocError> {
        let mut meta = self.meta.lock().unwrap();
        let bytes_needed = PAGE_SIZE * size_class.pages_per_span();
        if meta.next_page_offset + bytes_needed > self.memory_size {
            return Err(AllocError::OutOfMemory);
        }

        let span = Arc::new(MSpan::new(size_class, ObjectId(meta.next_page_offset)));

        // Вычисляем, какие страницы памяти занял этот спан
        let start_page_idx = meta.next_page_offset / PAGE_SIZE;
        let end_page_idx = start_page_idx + size_class.pages_per_span();

        // Регистрируем клон Arc в глобальной карте для каждой страницы спана
        for i in start_page_idx..end_page_idx {
            meta.page_map[i] = Some(Arc::clone(&span));
        }

        meta.next_page_offset += bytes_needed;
        Ok(span)
    }

    /// Записывает переданный объект (`ManagedObject`) в слот кучи по заданному `ObjectId`.
    ///
    /// Этот метод **не проверяет** права на слот — предполагается, что вызывающая сторона
    /// (аллокатор или GC) уже убедилась в его принадлежности.
    ///
    /// # Безопасность
    /// Вызывающий должен гарантировать, что в этот слот не происходит параллельной записи.
    pub fn write_to_slot(&self, obj_id: ObjectId, obj: &ManagedObject) {
        unsafe {
            // Извлекаем сырой указатель на мутабельный Vec
            let vec_ptr = self.memory.get();
            // Извлекаем абсолютный адрес из нашей структуры-обертки
            let base_ptr = (*vec_ptr).as_mut_ptr().add(obj_id.0);

            // 1. Записываем скрытый указатель на тип (первые 8 байт слота)
            let type_slot = base_ptr as *mut *const TypeDescriptor;
            type_slot.write(obj.type_desc_ptr);

            // 2. Копируем payload (ссылки + данные) сразу за ним
            let payload_dst = base_ptr.add(8);
            std::ptr::copy_nonoverlapping(obj.payload.as_ptr(), payload_dst, obj.payload.len());
        }
    }
    /// Читает метаданные и payload объекта из кучи по его `ObjectId`.
    ///
    /// Возвращает кортеж: (указатель на `TypeDescriptor`, срез байт payload).
    ///
    /// # Безопасность
    /// Вызывающий должен гарантировать, что слот валиден и не будет параллельно изменён.
    pub fn read_from_slot(&self, obj_id: ObjectId, size_class: MSpanSizeClass) -> (*const TypeDescriptor, &[u8]) {
        unsafe {
            let vec_ptr = self.memory.get();
            let base_ptr = (*vec_ptr).as_ptr().add(obj_id.0);

            let type_slot = base_ptr as *const *const TypeDescriptor;
            let type_desc_ptr = type_slot.read();

            // Получаем указатель на начало полезной нагрузки
            let payload_ptr = base_ptr.add(OBJ_HEADER_SIZE);

            // Размер payload — это размер всего блока минус 8 байт заголовка
            let payload_len = size_class.block_size() - 8;

            // 5. Формируем безопасный Rust-срез (slice) прямо поверх байт кучи
            let payload_slice = std::slice::from_raw_parts(payload_ptr, payload_len);

            (type_desc_ptr, payload_slice)
        }
    }

    /// Возвращает количество ссылочных полей (`refs_count`), объявленное в дескрипторе
    /// типа объекта. Используется сборщиком мусора во время обхода графа.
    pub fn read_refs_count(&self, obj_id: ObjectId) -> usize {
        unsafe {
            let vec_ptr = self.memory.get();
            let base_ptr = (*vec_ptr).as_ptr().add(obj_id.0);

            let type_slot = base_ptr as *const *const TypeDescriptor;
            let type_desc_ptr = type_slot.read();
            (*type_desc_ptr).refs_count
        }
    }

    /// Читает массив ссылок (`ObjectId`), хранящихся в первых `refs_count` восьмибайтовых
    /// полях payload-области. Используется GC при обходе достижимых объектов.
    pub fn read_references(&self, obj_id: ObjectId) -> &[u64] {
        let refs_count = self.read_refs_count(obj_id);
        unsafe {
            let vec_ptr = self.memory.get();
            let base_ptr = (*vec_ptr).as_ptr().add(obj_id.0);
            let payload_ptr = base_ptr.add(OBJ_HEADER_SIZE);
            std::slice::from_raw_parts(payload_ptr as *const u64, refs_count)
        }
    }

    /// Записывает новое значение ссылки в указанное поле объекта (по индексу).
    ///
    /// Используется, например, при перемещении объектов (write barrier).
    /// Вызывающий должен гарантировать корректность индекса.
    pub fn write_reference(&self, obj_id: ObjectId, field_index: usize, new_ref: ObjectId) {
        unsafe {
            let vec_ptr: *mut Vec<u8> = self.memory.get();
            let data_ptr: *mut u8 = (*vec_ptr).as_mut_ptr();
            let byte_offset = obj_id.0 + OBJ_HEADER_SIZE + (field_index * 8);
            let payload_ptr_u8 = data_ptr.add(byte_offset);
            let target_ptr = payload_ptr_u8 as *mut ObjectId;
            // for write barrier
            // let old_ref = target_ptr.read();
            target_ptr.write(new_ref);
        }
    }

    /// Возвращает количество записей в карте страниц (отладочная/вспомогательная функция).
    pub fn get_page_map_len(&self) -> usize {
        let meta = self.meta.lock().unwrap();
        meta.page_map.len()
    }

    /// Возвращает `Arc<MSpan>`, управляющий страницей с заданным индексом.
    ///
    /// Используется GC и аллокатором для поиска спана по `ObjectId` через номер страницы.
    pub fn get_span_for_page(&self, page_idx: usize) -> Option<Arc<MSpan>> {
        let meta = self.meta.lock().unwrap();
        if page_idx < meta.page_map.len() {
            meta.page_map[page_idx].clone() // Клонируем Arc, увеличивая счетчик ссылок
        } else {
            None
        }
    }
}