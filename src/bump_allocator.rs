use crate::allocator::{Allocator};
use crate::datastruct::{AllocError, ObjectHeader, ObjectId, GcColor, OBJECT_HEADER_SIZE, OBJECT_ID_SIZE};

pub struct BumpAllocator {
    /// Массив памяти, в который мы складываем аллоцированные нами объекты
    heap: Vec<u8>,
    /// Указатель на место, откуда начнется следующая аллокация (изначально 0)
    next: usize,
}

impl BumpAllocator {
    pub fn new(capacity: usize) -> Self {
        Self {heap: vec![0u8; capacity], next: 0}
    }
}

impl Allocator for BumpAllocator {
    /// Выделяет новый объект в куче.
    ///
    /// Метод НЕ принимает сами данные — он только резервирует непрерывный блок памяти
    /// под будущий объект и записывает его заголовок. Это разделение ответственности:
    /// аллокатор занимается только управлением памятью, а заполнение массива ссылок
    /// и полезных данных (payload) — задача более высокоуровневого кода.
    ///
    /// # Формат блока в памяти
    ///
    /// После успешной аллокации по смещению `object_id.0` в куче лежит:
    /// ```text
    /// [ObjectHeader (40 байт)] [References: ObjectId × refs_count] [Payload: aligned_payload байт]
    /// ```
    ///
    /// Массив ссылок и полезные данные ОСТАЮТСЯ НЕЗАПОЛНЕННЫМИ — это делает вызывающий код
    /// через отдельный write-метод.
    ///
    /// # Выравнивание payload
    ///
    /// `payload_size` выравнивается вверх до 8 байт по формуле `(payload_size + 7) & !7`.
    /// Это гарантирует, что следующий объект в куче начнётся по адресу, кратному 8,
    /// что критично для производительности процессора и корректной работы с #[repr(C)]-структурами.
    ///
    /// # Безопасность unsafe-блока
    ///
    /// Запись заголовка через `copy_nonoverlapping` в unsafe-блоке корректна, потому что:
    /// - Границы буфера проверены ДО входа в unsafe-блок (OutOfMemory при нехватке места).
    /// - Источник (заголовок на стеке) и назначение (байты кучи) гарантированно не пересекаются.
    /// - Запись идёт в выделенную и инициализированную нулями память `Vec<u8>`.
    ///
    /// # Возвращаемое значение
    ///
    /// Возвращает `ObjectId` — смещение в байтах от начала кучи, по которому располагается
    /// новый объект. Этот идентификатор можно использовать для чтения/обновления заголовка
    /// через `read_header` и `update_header`.
    ///
    /// # Ошибки
    ///
    /// - `AllocError::OutOfMemory` — если в куче недостаточно свободного места для
    ///   размещения блока размером `total_size`.
    fn alloc(&mut self, refs_count: usize, payload_size: usize) -> Result<ObjectId, AllocError> {
        let current_offset = self.next;
        let object_id = ObjectId(current_offset);
        let aligned_payload = (payload_size + 7) & !7;
        let total_size = OBJECT_HEADER_SIZE + aligned_payload + refs_count*OBJECT_ID_SIZE;
        if current_offset + total_size > self.heap.len() {
            return Err(AllocError::OutOfMemory);
        }
        let header = ObjectHeader{
            id: object_id,
            color: GcColor::White,
            size: total_size,
            refs_count,
            payload_size,
        };

        unsafe {
            let dst_ptr = self.heap.as_mut_ptr().add(current_offset) as *mut ObjectHeader;
            std::ptr::write(dst_ptr, header);
        }

        self.next += total_size;

        Ok(object_id)
    }

    /// Читает заголовок объекта из кучи по его `ObjectId`.
    ///
    /// Аллокатор не хранит отдельный реестр объектов — вся информация о них лежит
    /// непосредственно в байтах кучи. Этот метод достаёт заголовок оттуда и возвращает
    /// его безопасную копию на стеке.
    ///
    /// # Зачем нужно чтение заголовка
    ///
    /// Сборщик мусора постоянно читает заголовки:
    /// - Узнать `color` — жив ли объект, нужно ли его обходить.
    /// - Узнать `refs_count` — сколько ссылок искать за заголовком.
    /// - Узнать `size` — где начинается следующий объект в куче.
    ///
    /// # Формат хранения
    ///
    /// Заголовок лежит в сырых байтах кучи по смещению `object_id.0`.
    /// Физически это 40 байт с фиксированной [`#[repr(C)]`](datastruct::ObjectHeader) раскладкой полей.
    /// Мы читаем эти байты и интерпретируем их как `ObjectHeader`.
    ///
    /// # Безопасность unsafe-блока
    ///
    /// `std::ptr::read(src_ptr)` копирует байты из сырого указателя в локальную переменную.
    /// Это безопасно, потому что:
    /// - Границы буфера проверены ДО входа в unsafe: `offset + OBJECT_HEADER_SIZE <= heap.len()`.
    /// - Заголовок по этому смещению был ранее записан через `alloc` или `update_header` —
    ///   байты инициализированы и представляют валидную структуру.
    /// - `ObjectHeader: Copy` — операция битового копирования корректна (нет владения,
    ///   нет Drop, нет внутренних ссылок).
    /// - Выравнивание не нарушено: все аллокации кратны 8 байтам.
    ///
    /// # Ошибки
    ///
    /// - `AllocError::InvalidPointer` — если `object_id` указывает за границы кучи
    ///   или в область, где нет полного заголовка.
    fn read_header(&self, object_id: ObjectId) -> Result<ObjectHeader, AllocError> {
        if object_id.0 >= self.next {
            return Err(AllocError::InvalidPointer);
        }
        let offset = object_id.0;
        if offset + OBJECT_HEADER_SIZE > self.heap.len() {
            return Err(AllocError::InvalidPointer);
        }
        unsafe {
            let src_ptr = self.heap.as_ptr().add(offset) as *const ObjectHeader;
            let header = std::ptr::read(src_ptr);
            Ok(header)
        }
    }

    /// Обновляет заголовок существующего объекта в куче.
    ///
    /// Позволяет изменить метаданные уже выделенного объекта. Основной сценарий
    /// использования — GC меняет цвет объекта при трёхцветной маркировке:
    /// `White → Grey → Black`.
    ///
    /// # Почему нельзя просто мутировать заголовок на месте?
    ///
    /// Потому что заголовок — это не Rust-структура, а 40 байт в середине `Vec<u8>`.
    /// У нас нет `&mut ObjectHeader` на него — у нас есть только `ObjectId (смещение)`.
    /// Поэтому мы перезаписываем эти байты новой структурой через unsafe.
    ///
    /// # Что происходит
    ///
    /// 1. Проверяется, что `object_id` указывает на область внутри кучи, где может
    ///    поместиться целый заголовок.
    /// 2. Переданный `header` побайтово копируется в кучу по смещению `object_id.0`.
    /// 3. Старые байты заголовка затираются.
    ///
    /// Метод не меняет `next` — аллокация новых объектов не производится.
    ///
    /// # Безопасность unsafe-блока
    ///
    /// `std::ptr::copy_nonoverlapping` копирует 40 байт из стека в кучу.
    /// Это безопасно, потому что:
    /// - Границы проверены до unsafe.
    /// - Источник (стек) и назначение (куча) гарантированно не пересекаются.
    /// - Мы пишем поверх ранее записанных данных — структура остаётся валидной.
    ///
    /// # Ошибки
    ///
    /// - `AllocError::InvalidPointer` — если `object_id` выходит за границы кучи.
    fn update_header(&mut self, object_id: ObjectId, header: ObjectHeader) -> Result<(), AllocError> {
        let offset = object_id.0;
        if offset + OBJECT_HEADER_SIZE > self.heap.len() {
            return Err(AllocError::InvalidPointer);
        }
        unsafe {
            let dest_ptr = self.heap.as_mut_ptr().add(offset) as *mut ObjectHeader;
            std::ptr::copy_nonoverlapping(&header, dest_ptr, 1);
        }
        Ok(())
    }

    fn get_object_ptr(&mut self, object_id: ObjectId) -> Result<*mut u8, AllocError> {
        let offset = object_id.0;
        if offset + OBJECT_HEADER_SIZE > self.heap.len() {
            return Err(AllocError::InvalidPointer);
        }
        unsafe {
            let src_ptr = self.heap.as_mut_ptr().add(offset);
            Ok(src_ptr)
        }
    }

    fn read_references(&self, object_id: ObjectId) -> Result<Vec<ObjectId>, AllocError> {
        let header: ObjectHeader = self.read_header(object_id)?;
        let offset = object_id.0 + OBJECT_HEADER_SIZE;
        let refs_byte_size = header.refs_count * OBJECT_ID_SIZE;
        if offset + refs_byte_size > self.heap.len() {
            return Err(AllocError::InvalidPointer);
        }
        let mut references: Vec<ObjectId> = Vec::with_capacity(header.refs_count);
        unsafe {
            let src_ptr = self.heap.as_ptr().add(offset) as *const ObjectId;
            std::ptr::copy_nonoverlapping(src_ptr, references.as_mut_ptr(), header.refs_count);
            references.set_len(header.refs_count);
        }
        Ok(references)
    }
    
    fn over_next(&self, object_id: ObjectId) -> bool {
        object_id.0 >= self.next
    }
}