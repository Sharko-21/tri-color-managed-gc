use crate::datastruct::{AllocError, GcColor, ObjectId};
use crate::datastruct::GcColor::{Black, Grey, White};
use crate::managed_heap::ManagedHeap;

pub struct GarbageCollector {
    worklist: Vec<ObjectId>,
}

/// Реализация трёхцветного сборщика мусора (Tri-color Mark & Sweep).
///
/// `GarbageCollector` реализует классический алгоритм трассирующей сборки мусора,
/// который определяет достижимость объектов, начиная с набора корневых ссылок (roots),
/// и утилизирует недостижимые объекты.
///
/// # Трёхцветная абстракция (Tri-color Abstraction)
///
/// Алгоритм использует три цвета для маркировки объектов в процессе обхода графа:
///
/// - **White (Белый)**: Объект ещё не был обнаружен сборщиком. Если к концу фазы
///   трассировки объект остаётся белым — он недостижим из корней и подлежит очистке.
/// - **Grey (Серый)**: Объект обнаружен (достижим из корней), но его исходящие ссылки
///   ещё не просканированы. Серые объекты формируют рабочий фронт (worklist) для обхода.
/// - **Black (Чёрный)**: Объект и все его исходящие ссылки полностью просканированы.
///   Чёрные объекты гарантированно живы и не требуют повторного визита.
///
/// # Инварианты алгоритма
///
/// 1. На каждом шаге трассировки ни один чёрный объект не указывает на белый.
/// 2. Все серые объекты находятся в рабочем списке (`worklist`) и ждут обработки.
/// 3. После завершения фазы `trace` серых объектов не остаётся — все живые объекты
///    становятся чёрными, а недостижимые остаются белыми.
///
/// # Фазы сборки
///
/// Полный цикл сборки состоит из трёх последовательных фаз:
///
/// 1. **Инициализация (Initialize)** — все корневые объекты окрашиваются в серый
///    и помещаются в рабочий список.
/// 2. **Трассировка (Trace)** — итеративный обход графа: для каждого серого объекта
///    его дочерние ссылки красятся в серый, а сам объект — в чёрный.
/// 3. **Очистка (Sweep)** — линейный проход по всей куче: белые объекты признаются
///    мусором (их связи обнуляются), чёрные возвращаются в белый цвет для следующего цикла.
///
/// # Ограничения текущей реализации
///
/// - Сборка выполняется полностью синхронно (Stop-The-World). Конкурентная версия
///   потребует внедрения write barrier и фоновых потоков.
/// - Bump-аллокатор не умеет переиспользовать память, освобождённую в фазе sweep.
///   Освобождённые объекты лишь обнуляют свои ссылки, но блоки памяти остаются занятыми.
///   Для реального переиспользования необходим free-list аллокатор или компактификация.
impl GarbageCollector {
    pub fn new() -> Self {
        return Self { worklist: Vec::new() };
    }

    pub fn initialize(&mut self, managed_heap: &mut ManagedHeap, roots: &[ObjectId]) -> Result<(), AllocError> {
        for root_id in roots {
            let mut header = managed_heap.read_header(*root_id)?;
            if header.color != White {
                continue;
            }
            header.color = GcColor::Grey;
            managed_heap.update_header(header)?;
            self.worklist.push(root_id.clone());
        }
        Ok(())
    }

    pub fn trace(&mut self, managed_heap: &mut ManagedHeap) -> Result<(), AllocError> {
        while let Some(object_id) = self.worklist.pop() {
            let mut header = managed_heap.read_header(object_id)?;
            let references = managed_heap.read_references(object_id)?;

            for reference in references {
                let mut reference = managed_heap.read_header(reference)?;
                if reference.color == Black || reference.color == Grey {
                    continue;
                }
                reference.color = Grey;
                managed_heap.update_header(reference)?;
                self.worklist.push(reference.id);
            }
            header.color = Black;
            managed_heap.update_header(header)?;
        }
        Ok(())
    }

    pub fn sweep(&mut self, managed_heap: &mut ManagedHeap) -> Result<(), AllocError> {
        let mut object_id = ObjectId(0);
        loop {
            if managed_heap.over_next(object_id) {
                break;
            }
            let mut header = managed_heap.read_header(object_id)?;
            match header.color {
                White => {
                    header.refs_count = 0;
                    managed_heap.update_header(header)?;
                }
                Black => {
                    header.color = White;
                    managed_heap.update_header(header)?;
                },
                Grey => {}
            }

            object_id = ObjectId(header.id.0 + header.size);
        }
        Ok(())
    }

    pub fn collect(&mut self, heap: &mut ManagedHeap, roots: &[ObjectId]) -> Result<(), AllocError> {
        self.initialize(heap, roots)?;
        self.trace(heap)?;
        self.sweep(heap)?;
        Ok(())
    }
}