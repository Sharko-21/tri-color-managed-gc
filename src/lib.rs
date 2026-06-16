pub mod datastruct;
pub mod allocator;
pub mod bump_allocator;
pub mod managed_heap;
pub mod garbage_collector;

#[cfg(test)]
mod tests {
    use crate::datastruct::{GcColor, ManagedObject, ObjectId};
    use crate::garbage_collector::GarbageCollector;
    use crate::managed_heap::ManagedHeap;

    #[test]
    fn test_heap_serialization_and_alignment() {
        // Создаем кучу скромного размера — 256 байт
        let mut heap = ManagedHeap::new(256);

        // --- ТЕСТ 1: Простой объект с выровненным payload ---
        let obj1 = ManagedObject {
            id: ObjectId(0), // Аллокатор сам назначит правильный id, здесь пишем заглушку
            references: vec![ObjectId(10), ObjectId(20)], // 2 ссылки = 16 байт
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8],        // 8 байт (уже выровнено)
        };
        // Итого размер: 40 (заголовок) + 16 (ссылки) + 8 (payload) = 64 байта.

        let id1 = heap.write_object(&obj1).expect("Failed to write obj1");
        assert_eq!(id1.0, 0, "Первый объект должен начинаться с начала кучи (смещение 0)");

        // Читаем обратно и проверяем данные
        let read_obj1 = heap.read_object(id1).expect("Failed to read obj1");
        assert_eq!(read_obj1.references, obj1.references);
        assert_eq!(read_obj1.payload, obj1.payload);


        // --- ТЕСТ 2: Объект с НЕВЫРОВНЕННЫМ payload (Проверка Padding) ---
        let obj2 = ManagedObject {
            id: ObjectId(0),
            references: vec![],                  // 0 ссылок = 0 байт
            payload: vec![0xAA, 0xBB, 0xCC],     // 3 байта (должно округлиться до 8 байт!)
        };
        // Итого размер: 40 (заголовок) + 0 (ссылки) + 8 (выровненный payload) = 48 байт.

        let id2 = heap.write_object(&obj2).expect("Failed to write obj2");

        // ВАЖНАЯ ПРОВЕРКА: смещение второго объекта должно быть ровно 64 (размер первого объекта)
        assert_eq!(id2.0, 64, "Второй объект должен лечь сразу за первым");

        let read_obj2 = heap.read_object(id2).expect("Failed to read obj2");
        assert_eq!(read_obj2.references, obj2.references);
        assert_eq!(read_obj2.payload, obj2.payload); // Длина вектора должна остаться 3!


        // --- ТЕСТ 3: Проверка корректности смещения третьего объекта ---
        let obj3 = ManagedObject {
            id: ObjectId(0),
            references: vec![id1, id2], // Ссылаемся на предыдущие объекты
            payload: vec![42],          // 1 байт -> округлится до 8
        };
        // Итого размер: 40 + 16 + 8 = 64 байта.
        let id3 = heap.write_object(&obj3).expect("Failed to write obj3");

        // Если паддинг во втором объекте отработал верно, то: 64 (id2) + 48 (размер obj2) = 112
        assert_eq!(id3.0, 112, "Третий объект сместился неверно. Проблема с выравниванием payload у obj2!");

        let read_obj3 = heap.read_object(id3).expect("Failed to read obj3");
        assert_eq!(read_obj3.references, vec![id1, id2]);
        assert_eq!(read_obj3.payload, vec![42]);


        // --- ТЕСТ 4: Проверка Out Of Memory ---
        // Текущий next = 112 + 64 = 176. Свободно: 256 - 176 = 80 байт.
        // Пытаемся засунуть огромный объект, который не пролезет.
        let big_obj = ManagedObject {
            id: ObjectId(0),
            references: vec![],
            payload: vec![0; 100], // Заголовок 40 + 104 (выровненный payload) = 144 байта.
        };

        let oom_result = heap.write_object(&big_obj);
        assert!(oom_result.is_err(), "Аллокатор должен был вернуть ошибку OutOfMemory");
    }

    #[test]
    fn test_garbage_collector_lifecycle() {
        // Создаем чистую кучу на 256 байт и инстанс GC
        let mut heap = ManagedHeap::new(256);
        let mut gc = GarbageCollector::new();

        // --- 1. Аллокация живого объекта (Корень) ---
        let root_obj = ManagedObject {
            id: ObjectId(0),
            references: vec![],
            payload: vec![1, 2, 3, 4], // 4 байта -> выравнивание до 8
        }; // Размер: 40 + 0 + 8 = 48 байт
        let root_id = heap.write_object(&root_obj).expect("Failed to write root_obj");

        // --- 2. Аллокация циклического мусора ---
        // Создаем два объекта, которые изначально указывают на какие-то ID,
        // имитируя взаимные ссылки в графе (например, id=48 ссылается на id=96, а id=96 на id=48)
        // В твоей куче:
        // root_obj займет смещение 0..48.
        // cyclic1 займет смещение 48..96 (40 заголовок + 8 ссылка + 0 payload = 48 байт).
        // cyclic2 займет смещение 96..144 (40 заголовок + 8 ссылка + 0 payload = 48 байт).

        let cyclic1_id = ObjectId(48);
        let cyclic2_id = ObjectId(96);

        let cyclic1_obj = ManagedObject {
            id: ObjectId(0),
            references: vec![cyclic2_id], // Ссылается на второй объект
            payload: vec![],
        };
        let id_c1 = heap.write_object(&cyclic1_obj).expect("Failed to write cyclic1");
        assert_eq!(id_c1, cyclic1_id, "Проверка топологии: cyclic1 лег не на свое расчетное смещение");

        let cyclic2_obj = ManagedObject {
            id: ObjectId(0),
            references: vec![cyclic1_id], // Ссылается на первый объект. Цикл замкнулся!
            payload: vec![],
        };
        let id_c2 = heap.write_object(&cyclic2_obj).expect("Failed to write cyclic2");
        assert_eq!(id_c2, cyclic2_id, "Проверка топологии: cyclic2 лег не на свое расчетное смещение");

        // --- 3. Верификация состояния до сборки мусора ---
        // Проверяем, что все три объекта изначально находятся в состоянии White
        assert_eq!(heap.read_header(root_id).unwrap().color, GcColor::White);
        assert_eq!(heap.read_header(cyclic1_id).unwrap().color, GcColor::White);
        assert_eq!(heap.read_header(cyclic2_id).unwrap().color, GcColor::White);

        // Убеждаемся, что счетчик ссылок у мусорного объекта равен 1
        assert_eq!(heap.read_header(cyclic1_id).unwrap().refs_count, 1);

        // --- 4. Запуск сборщика мусора (collect) ---
        // Передаем в качестве корней ТОЛЬКО root_id.
        // Циклическая группа [cyclic1_id, cyclic2_id] изолирована и недостижима из корней.
        let roots = vec![root_id];
        gc.collect(&mut heap, &roots).expect("GC collection failed");

        // --- 5. Проверка инвариантов после сборки мусора ---
        let root_header = heap.read_header(root_id).unwrap();
        let c1_header = heap.read_header(cyclic1_id).unwrap();
        let c2_header = heap.read_header(cyclic2_id).unwrap();

        // А. Живой объект должен пережить трассировку и фаза sweep обязана сбросить его обратно в White
        assert_eq!(root_header.color, GcColor::White, "Живой объект потерял маркер доступности");

        // Б. Мусорные объекты не были достигнуты, фаза sweep должна была:
        //    1. Оставить их White.
        //    2. ОБНУЛИТЬ их refs_count, разорвав циклическую зависимость в метаданных кучи.
        assert_eq!(c1_header.color, GcColor::White);
        assert_eq!(c2_header.color, GcColor::White);

        assert_eq!(c1_header.refs_count, 0, "GC не очистил связи у мертвого объекта cyclic1");
        assert_eq!(c2_header.refs_count, 0, "GC не очистил связи у мертвого объекта cyclic2");
    }
}