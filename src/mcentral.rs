use std::sync::{Arc, Mutex};
use crate::mspan::{MSpan, MSpanSizeClass};
use strum::EnumCount;

pub struct SizeClassBin {
    // Спаны, в которых еще есть свободные слоты для аллокации
    pub partial: Vec<Arc<MSpan>>,
    // Полностью заполненные спаны, которые мы удерживаем для GC
    pub full: Vec<Arc<MSpan>>,
}

/// MCentral реализует паттерн Sharded Lock (разделяемая блокировка).
/// Вместо одного глобального Mutex на всю структуру, каждый класс размера
/// имеет свой изолированный бакет под собственным Mutex.
pub struct MCentral {
    pub bins: [Mutex<SizeClassBin>; MSpanSizeClass::COUNT],
}

impl MCentral {
    pub fn new() -> Self {
        // Инициализируем массив бакетов для каждого класса размеров индивидуальным Mutex.
        let mut bins = Vec::with_capacity(MSpanSizeClass::COUNT);
        for _ in 0..MSpanSizeClass::COUNT {
            bins.push(Mutex::new(SizeClassBin {
                partial: Vec::new(),
                full: Vec::new(),
            }));
        }

        let bins_array: [Mutex<SizeClassBin>; MSpanSizeClass::COUNT] = bins
            .try_into()
            .unwrap_or_else(|_| panic!("Не удалось инициализировать MCentral bins"));

        MCentral { bins: bins_array }
    }

    /// Запросить свободный спан для конкретного класса размера.
    /// Операция гарантированно выполняется за O(1), так как мы просто достаем спан из списка partial.
    /// Блокирует ТОЛЬКО бакет этого класса размера.
    pub fn pop_span(&self, size_class: MSpanSizeClass) -> Option<Arc<MSpan>> {
        let idx = size_class.to_index();
        let mut bin = self.bins[idx].lock().unwrap();
        bin.partial.pop()
    }

    /// Вернуть заполненный спан обратно в бакет.
    /// Блокирует ТОЛЬКО бакет этого класса размера.
    /// Вернуть спан обратно в бакет.
    /// Метод автоматически распределяет его в список full или partial в зависимости от заполненности.
    pub fn push_span(&self, size_class: MSpanSizeClass, span: Arc<MSpan>) {
        let idx = size_class.to_index();
        let mut bin = self.bins[idx].lock().unwrap();
        if span.is_full() {
            bin.full.push(span);
        } else {
            bin.partial.push(span);
        }
    }

    pub fn sweep_full_spans(&self) {
        for bin_mutex in &self.bins {
            // Блокируем текущий бакет
            let mut bin = bin_mutex.lock().unwrap();

            // Разделяем структуру на отдельные mut-ссылки на поля,
            // чтобы borrow checker не ругался на одновременное изменение bin
            let SizeClassBin { partial, full } = &mut *bin;

            // Метод retain_mut проходит по вектору:
            // Если функция возвращает true — элемент остается в full.
            // Если false — элемент удаляется из full.
            full.retain_mut(|span| {
                if !span.is_full() {
                    // Если спан больше не заполнен, клонируем Arc (увеличиваем счетчик ссылок)
                    // и перемещаем его в список частичных
                    partial.push(Arc::clone(span));
                    false // Удаляем из full
                } else {
                    true // Оставляем в full
                }
            });
        }
    }
}