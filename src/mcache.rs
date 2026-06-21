//! Модуль локального кэша аллокации (MCache).
//!
//! Каждый поток, выполняющий аллокацию, владеет собственным экземпляром `MCache`.
//! Кэш хранит по одному активному `MSpan` для каждого класса размеров (`MSpanSizeClass`).
//! Аллокация из этого спана выполняется без каких-либо блокировок.
//! Когда спан заполняется, он возвращается в центральный пул (`MCentral`),
//! а из пула или напрямую из кучи запрашивается новый спан.
//!
//! Такая архитектура (per-thread cache + central free list) повторяет идею
//! аллокатора Go и позволяет достичь высокой пропускной способности в многопоточных сценариях.

use std::sync::{Arc};
use crate::mspan::{MSpan, MSpanSizeClass};
use strum::EnumCount;
use crate::datastruct::{AllocError, ManagedObject, ObjectId};
use crate::managed_heap::ManagedHeap;
use crate::mcentral::MCentral;

/// Локальный кэш аллокации одного потока.
///
/// Содержит заранее закреплённый (или взятый из глобального пула) спан для каждого класса
/// размера объектов. Аллокация происходит напрямую из этого спана через атомарные битовые
/// маски — никакие блокировки на этом пути не требуются.
///
/// ## Потокобезопасность
///
/// Сам `MCache` не реализует `Sync`, так как его методы принимают `&mut self`.
/// Каждый поток создаёт собственный экземпляр `MCache`, поэтому гонок внутри кэша нет.
/// Общие ресурсы (`MCentral`, `ManagedHeap`) защищены изнутри (мьютексы, атомики).
pub struct MCache {
    /// Массив активных спанов для каждого класса размеров.
    /// `None` означает, что спан ещё не выделен (первая аллокация или спан полностью
    /// заполнен и отправлен обратно в `MCentral`).
    pub alloc_spans: [Option<Arc<MSpan>>; MSpanSizeClass::COUNT],
    /// Ссылка на центральный пул спанов (разделяется между всеми потоками).
    central: Arc<MCentral>,
    /// Ссылка на управляемую кучу (разделяется между всеми потоками).
    heap: Arc<ManagedHeap>,
}

impl MCache {
    pub fn new(central: Arc<MCentral>, heap: Arc<ManagedHeap>) -> Self {
        const INIT_OPTION: Option<Arc<MSpan>> = None;

        MCache {
            alloc_spans: [INIT_OPTION; MSpanSizeClass::COUNT],
            central,
            heap,
        }
    }

    /// Выделяет место в куче под объект и записывает его payload.
    ///
    /// Это основной метод, используемый рантаймом для создания новых объектов.
    /// Он выбирает подходящий по размеру класс, находит или создаёт спан,
    /// находит свободный блок в спане, записывает данные через `ManagedHeap::write_to_slot`,
    /// помечает блок как занятый, и при необходимости возвращает заполненный спан
    /// обратно в глобальный пул.
    pub fn alloc(
        &mut self,
        size_class: MSpanSizeClass,
        obj: &ManagedObject,
    ) -> Result<ObjectId, AllocError> {
        let index = size_class.to_index();

        let span = match self.alloc_spans[index].take() {
            Some(span) => span,
            None => {
                match self.central.pop_span(size_class) {
                    Some(s) => s,
                    None => self.heap.grow_span(size_class)?,
                }
            }
        };

        let object_id = span
            .find_next_block()
            .expect("Критическая ошибка: в локальном кэше оказался заполненный MSpan!");

        // Быстрый путь (Lock-Free) записи в память
        self.heap.write_to_slot(object_id, obj);
        span.publish_block(object_id);

        if span.is_full() {
            self.central.push_span(size_class, span);
        } else {
            self.alloc_spans[index] = Some(span);
        }

        Ok(object_id)
    }
}