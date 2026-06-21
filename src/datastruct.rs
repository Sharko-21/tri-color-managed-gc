/// Представляет собой виртуальный указатель внутри управляемой памяти.///
/// Внутреннее значение `usize` — это строгое смещение (offset) в байтах
/// от начала базового массива нашей кучи.
///
/// Разрешенные дериваты необходимы, чтобы тип вел себя как обычное число:
/// передавался по значению (копировался), сравнивался и мог выступать в качестве
/// ключа в структурах данных вроде `HashMap`.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct ObjectId(pub usize);

#[derive(Debug, Clone)]
pub struct TypeDescriptor {
    pub total_size: usize,        // Физический размер объекта (вместе с дескриптором типа)
    pub refs_count: usize,        // Сколько первых полей по 8 байт являются ObjectId
}

pub const OBJ_HEADER_SIZE: usize = size_of::<*const TypeDescriptor>(); // 8 байт

#[derive(Debug, Clone)]
pub struct ManagedObject {
    pub type_desc_ptr: *const TypeDescriptor,
    pub payload: Vec<u8>, // Здесь ссылки и чистые данные лежат вместе плоским байтовым массивом
}

pub const OBJECT_ID_SIZE: usize = size_of::<ObjectId>();

/// Набор маркеров (цветов) для реализации трехцветного алгоритма маркировки (Tri-color Abstraction).
///
/// Используется фоновыми потоками сборщика мусора для инкрементной/конкурентной разметки графа
/// без полной остановки приложения (Stop-The-World).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GcColor {
    /// Объект еще не обнаружен сборщиком мусора. Если к концу фазы маркировки
    /// объект остается белым — он признается мусором и подлежит утилизации.
    White,
    /// Объект обнаружен и признан живым, но его дочерние ссылки еще не исследованы.
    /// Серые объекты формируют фронт работы для фоновых воркеров GC.
    Grey,
    /// Объект и все его дочерние ссылки полностью исследованы сборщиком мусора.
    Black,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
    InvalidPointer,
}

pub const PAGE_SIZE: usize = 4096;