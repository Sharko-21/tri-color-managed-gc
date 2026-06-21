use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use crate::datastruct::{ObjectId, PAGE_SIZE};
use crate::managed_heap::ManagedHeap;
use crate::mcentral::MCentral;

pub struct GarbageCollector {
    worklist: Vec<ObjectId>,
    is_marking: AtomicBool,
    barrier_worklist: Mutex<Vec<ObjectId>>,
}

impl GarbageCollector {
    pub fn new() -> GarbageCollector {
        GarbageCollector { worklist: Vec::new(), is_marking: AtomicBool::new(false), barrier_worklist: Mutex::new(Vec::new()) }
    }

    fn mark_object(&self, obj_id: ObjectId, heap: &ManagedHeap) -> bool {
        let offset = obj_id.0;
        // если по какой-то причине object_id ссылается на нашу служебную страницу (первая страница
        // служит чисто в служебных целях)
        if offset < PAGE_SIZE {
            return false;
        }
        let page_num = offset / PAGE_SIZE;
        if page_num >= heap.get_page_map_len() {
            return false;
        }
        let span = match heap.get_span_for_page(page_num) {
            Some(span) => span,
            None => return false,
        };
        let span_start = span.start_id.0;
        if offset < span_start {
            return false;
        }
        let offset_per_span = offset - span_start;
        let block_size = span.size_class.block_size();
        // ДОПОЛНИТЕЛЬНАЯ ЗАЩИТА: проверяем, указывает ли ObjectId строго на начало блока
        if offset_per_span % block_size != 0 {
            return false; // Указатель смещен внутрь объекта или поврежден
        }
        let index_per_span = offset_per_span / block_size;
        // ЗАЩИТА ОТ ВЫХОДА ЗА ГРАНИЦЫ: проверяем емкость спана
        if index_per_span >= span.total_blocks {
            return false;
        }
        let mask_idx = index_per_span / 64;
        let bit_pos = index_per_span % 64;
        let mask = 1 << bit_pos;

        // Атомарно взводим бит. fetch_or возвращает СТАРОЕ значение слова.
        // Используем Release, чтобы гарантировать видимость наших действий другим потокам.
        let old_val = span.marked_bits[mask_idx].fetch_or(mask, Ordering::Release);

        // Если в старом значении на этой позиции был 0 — объект был Белым.
        // Мы его успешно покрасили, возвращаем true (нужно добавить в worklist).
        // Если там уже была 1 — объект Черный или Серый, возвращаем false (игнорируем).
        (old_val & mask) == 0
    }

    pub fn initialize(&mut self, roots: &[ObjectId], heap: &ManagedHeap) {
        for root in roots {
            if self.mark_object(*root, heap) {
                self.worklist.push(*root);
            }
        }
    }

    pub fn trace(&mut self, heap: &ManagedHeap) {
        while let Some(obj_id) = self.worklist.pop() {
            let refs = heap.read_references(obj_id);
            for r in refs {
                let child_id = ObjectId(*r as usize);
                let marked = self.mark_object(child_id, heap);
                if marked {
                    self.worklist.push(child_id);
                }
            }
        }
    }

    pub fn sweep(&mut self, heap: &ManagedHeap, central: &MCentral) {
        let mut i = 0;
        while i < heap.get_page_map_len() {
            match heap.get_span_for_page(i) {
                Some(span) => {
                    span.sweep();
                    i += span.size_class.pages_per_span();
                }
                None => i+=1,
            }
        }
        central.sweep_full_spans()
    }

    pub fn start_marking(&self) {
        self.is_marking.store(true, Ordering::Release);
    }

    pub fn stop_marking(&self) {
        self.is_marking.store(false, Ordering::Release);
    }

    pub fn drain_barrier_worklist(&self) -> Vec<ObjectId> {
        let mut list = self.barrier_worklist.lock().unwrap();
        std::mem::take(&mut *list)
    }
}
