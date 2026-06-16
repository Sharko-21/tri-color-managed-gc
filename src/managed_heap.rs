use crate::allocator::Allocator;
use crate::bump_allocator::BumpAllocator;
use crate::datastruct::{AllocError, ManagedObject, ObjectHeader, ObjectId, OBJECT_HEADER_SIZE, OBJECT_ID_SIZE};


/// Высокоуровневая прослойка (Facade) над плоской бинарной кучей.
///
/// `ManagedHeap` — это единственная точка входа для приложения при работе с управляемой памятью.
/// Она скрывает за собой низкоуровневый аллокатор (сейчас `BumpAllocator`) и берёт на себя
/// всю сериализацию/десериализацию объектов: запись заголовка, массива ссылок и payload
/// в сырые байты кучи, а также обратное чтение в безопасную структуру `ManagedObject`.
///
/// # Разделение ответственности
///
/// - **Allocator** (например, `BumpAllocator`) отвечает только за управление памятью:
///   выделение блока, чтение и обновление заголовка, проверка границ.
/// - **ManagedHeap** отвечает за наполнение выделенного блока данными:
///   копирование массива ссылок и payload в нужные смещения относительно заголовка.
///
/// Такой подход позволяет в будущем заменить `BumpAllocator` на более сложный аллокатор
/// (например, с дефрагментацией или поколенческой сборкой), не меняя логику сериализации.
pub struct ManagedHeap {
    allocator: BumpAllocator,
}

impl ManagedHeap {
    /// Создаёт новую управляемую кучу заданного размера.
    ///
    /// # Что происходит
    ///
    /// 1. Под капотом создаётся `Vec<u8>` ёмкостью `heap_size` байт,
    ///    заполненный нулями — это наша плоская бинарная куча.
    /// 2. Указатель следующей аллокации (`next`) устанавливается в `0`.
    ///
    /// # Аргументы
    ///
    /// - `heap_size` — общий размер кучи в байтах. Вся последующая работа
    ///   с памятью ограничена этим объёмом. При попытке выделить больше
    ///   аллокатор вернёт `AllocError::OutOfMemory`.
    ///
    /// # Пример
    ///
    /// ```ignore
    /// let mut heap = ManagedHeap::new(256);
    /// ```
    pub fn new(heap_size: usize) -> Self {
        Self {allocator: BumpAllocator::new(heap_size)}
    }
    /// Сериализует управляемый объект в кучу и возвращает его идентификатор.
    ///
    /// Метод принимает высокоуровневое представление объекта (`ManagedObject`),
    /// выделяет под него непрерывный блок памяти через аллокатор и копирует
    /// все данные (заголовок уже записан аллокатором, ссылки и payload дописываем здесь)
    /// в сырые байты кучи.
    ///
    /// # Структура блока после записи
    ///
    /// После успешной записи по смещению `object_id.0` в куче лежит:
    /// ```text
    /// [ObjectHeader (40 байт)] [References: ObjectId × refs_count] [Payload: aligned_payload байт]
    /// ```
    ///
    /// Где:
    /// - **ObjectHeader** — уже записан аллокатором на этапе `alloc`.
    /// - **References** — копия `obj.references` (массив `ObjectId`).
    /// - **Payload** — копия `obj.payload` (сырые байты пользовательских данных).
    ///
    /// # Почему заголовок записывается отдельно от ссылок и payload?
    ///
    /// Это разделение ответственности: аллокатор (`BumpAllocator::alloc`) резервирует блок
    /// и пишет только заголовок — ему не нужно знать про внутреннюю структуру данных.
    /// А `ManagedHeap::write_object` уже знает, что за заголовком идут ссылки, а за ними —
    /// полезные данные, и заполняет эти области.
    ///
    /// # Выравнивание
    ///
    /// Выравнивание payload до 8 байт уже выполнено аллокатором на этапе расчёта
    /// `total_size` (формула `(payload_size + 7) & !7`). Здесь мы копируем ровно
    /// `obj.payload.len()` байт — паддинговые байты остаются нулевыми (куча инициализирована нулями).
    ///
    /// # Безопасность unsafe-блока
    ///
    /// Внутри используется два вызова `std::ptr::copy_nonoverlapping`:
    ///
    /// 1. **Копирование ссылок**: пишет `refs_count` элементов `ObjectId`
    ///    сразу за заголовком (смещение `OBJECT_HEADER_SIZE`).
    /// 2. **Копирование payload**: пишет `payload_size` сырых байт
    ///    сразу за массивом ссылок (смещение `OBJECT_HEADER_SIZE + refs_count * OBJECT_ID_SIZE`).
    ///
    /// Это безопасно, потому что:
    /// - Аллокатор уже проверил, что в куче достаточно места для всего блока
    ///   (иначе `alloc` вернул бы `Err` и мы бы не дошли до unsafe).
    /// - Источники (`obj.references.as_ptr()`, `obj.payload.as_ptr()`) и назначения
    ///   (смещения внутри кучи) гарантированно не пересекаются.
    /// - Размеры копируемых областей не превышают границ выделенного блока.
    ///
    /// # Возвращаемое значение
    ///
    /// Возвращает `ObjectId` — смещение в байтах от начала кучи, по которому
    /// был записан объект. Этот идентификатор можно использовать для чтения объекта
    /// обратно через `read_object`.
    ///
    /// # Ошибки
    ///
    /// Пробрасывает ошибки от аллокатора:
    /// - `AllocError::OutOfMemory` — если в куче недостаточно свободного места.
    pub fn write_object(&mut self, obj: &ManagedObject) -> Result<ObjectId, AllocError> {
        let id: ObjectId = self.allocator.alloc(obj.references.len(), obj.payload.len())?;
        let references_offset = OBJECT_HEADER_SIZE;
        let base_ptr = self.allocator.get_object_ptr(id)?;
        unsafe {
            let references_ptr = base_ptr.add(references_offset) as *mut ObjectId;
            std::ptr::copy_nonoverlapping(obj.references.as_ptr(), references_ptr, obj.references.len());

            let refs_bytes_size = obj.references.len() * OBJECT_ID_SIZE;
            let payload_ptr = base_ptr.add(OBJECT_HEADER_SIZE + refs_bytes_size);
            std::ptr::copy_nonoverlapping(obj.payload.as_ptr(), payload_ptr, obj.payload.len());
        }
        Ok(id)
    }

    /// Десериализует объект из кучи по его идентификатору.
    ///
    /// Читает сырые байты по указанному смещению, интерпретирует заголовок,
    /// а затем на основе его метаданных (`refs_count` и `payload_size`) вычитывает
    /// массив ссылок и полезные данные, собирая всё в безопасную структуру `ManagedObject`.
    ///
    /// # Что происходит внутри
    ///
    /// 1. **Чтение заголовка**: через `allocator.read_header(object_id)` получаем `ObjectHeader`,
    ///    из которого узнаём количество ссылок (`refs_count`) и размер payload (`payload_size`).
    /// 2. **Чтение ссылок**: начиная со смещения `OBJECT_HEADER_SIZE` копируем
    ///    `refs_count` элементов `ObjectId` во временный вектор.
    /// 3. **Чтение payload**: начиная со смещения `OBJECT_HEADER_SIZE + refs_count * OBJECT_ID_SIZE`
    ///    копируем `payload_size` сырых байт во временный вектор.
    /// 4. **Сборка результата**: упаковываем всё в `ManagedObject` и возвращаем.
    ///
    /// # Формат чтения
    ///
    /// Чтение строго соответствует формату записи:
    /// ```text
    /// [ObjectHeader (40 байт)] [References: ObjectId × refs_count] [Payload: payload_size байт]
    /// ```
    ///
    /// # Зачем нужно копировать данные из кучи, а не возвращать ссылку?
    ///
    /// Потому что наша куча — это `Vec<u8>`, а не арена с адресуемыми Rust-объектами.
    /// Ссылка на байты внутри `Vec<u8>` была бы неудобна и опасна: мутация кучи
    /// (новая аллокация) может вызвать реаллокацию `Vec`, инвалидируя ссылку.
    /// Поэтому мы всегда копируем данные в свежий `ManagedObject` на стеке/куче приложения.
    ///
    /// # Безопасность unsafe-блока
    ///
    /// Два блока unsafe:
    ///
    /// 1. **Чтение ссылок**: `copy_nonoverlapping` копирует `refs_count * size_of::<ObjectId>()`
    ///    байт из кучи в only-что-созданный `Vec<ObjectId>`. Безопасно, потому что:
    ///    - `Vec::with_capacity` выделил ровно столько памяти, сколько нужно.
    ///    - Проверка границ выполнена в `allocator.get_object_ptr` и `read_header`.
    ///    - `set_len` корректен, так как байты по этому смещению валидны (записаны через `write_object`).
    ///
    /// 2. **Чтение payload**: аналогично, копирует ровно `payload_size` байт.
    ///    Длина вектора payload устанавливается через `set_len` — это корректно, так как
    ///    исходные байты в куче валидны и представляют собой массив `u8`.
    ///
    /// # Возвращаемое значение
    ///
    /// Возвращает `ManagedObject` — безопасную копию данных, извлечённых из кучи.
    /// Векторы `references` и `payload` — независимые выделения памяти, не связанные с кучей.
    ///
    /// # Ошибки
    ///
    /// Пробрасывает ошибки от аллокатора:
    /// - `AllocError::InvalidPointer` — если `object_id` указывает за границы кучи.
    pub fn read_object(&mut self, object_id: ObjectId) -> Result<ManagedObject, AllocError> {
        let header = self.allocator.read_header(object_id)?;
        let base_ptr = self.allocator.get_object_ptr(object_id)?;

        // Заполняем сначала references объекта
        let references_offset = OBJECT_HEADER_SIZE;
        let mut references: Vec<ObjectId> = Vec::with_capacity(header.refs_count);
        unsafe {
            let src_ptr = base_ptr.add(references_offset) as *const ObjectId;
            std::ptr::copy_nonoverlapping(src_ptr, references.as_mut_ptr(), header.refs_count);
            references.set_len(header.refs_count);
        }

        // Теперь заполяем сами данные объекта
        let mut payload: Vec<u8> = Vec::with_capacity(header.payload_size);
        let payload_offset = references_offset + (header.refs_count * OBJECT_ID_SIZE);
        unsafe {
            let src_ptr = base_ptr.add(payload_offset);
            std::ptr::copy_nonoverlapping(src_ptr, payload.as_mut_ptr(), header.payload_size);
            payload.set_len(header.payload_size);
        }

        Ok(ManagedObject{
            id: object_id,
            references,
            payload,
        })
    }

    pub fn read_header(&mut self, object_id: ObjectId) -> Result<ObjectHeader, AllocError> {
        self.allocator.read_header(object_id)
    }

    pub fn update_header(&mut self, header: ObjectHeader) -> Result<(), AllocError> {
        self.allocator.update_header(header.id, header)
    }

    pub fn read_references(&mut self, object_id: ObjectId) -> Result<Vec<ObjectId>, AllocError> {
        self.allocator.read_references(object_id)
    }

    pub fn over_next(&self, object_id: ObjectId) -> bool {
        self.allocator.over_next(object_id)
    }
}