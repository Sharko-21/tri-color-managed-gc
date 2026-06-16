use criterion::{criterion_group, criterion_main, Criterion};
// Подставь имя своего крейта из Cargo.toml
use managed_gc::datastruct::{ManagedObject, ObjectId};
use managed_gc::managed_heap::ManagedHeap;
use managed_gc::garbage_collector::GarbageCollector;

fn bench_gc_collect(c: &mut Criterion) {
    c.bench_function("gc_collect_3_objects", |b| {
        b.iter_with_setup(
            || {
                // На куждую итерацию бенчмарка Criterion будет готовить ЧИСТУЮ кучу.
                // Это важно, чтобы замерять честный проход, а не работу по уже зачищенной куче.
                let mut heap = ManagedHeap::new(65536);
                let gc = GarbageCollector::new();

                // 1. Создаем живой корень
                let root_obj = ManagedObject {
                    id: ObjectId(0),
                    references: vec![],
                    payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
                };
                let root_id = heap.write_object(&root_obj).unwrap();

                // 2. Создаем циклический мусор (они ссылаются друг на друга по жестким смещениям)
                let cyclic1_id = ObjectId(64); // 40 (header) + 16 (refs) + 8 (payload)
                let cyclic2_id = ObjectId(112); // 40 (header) + 8 (refs) + 0 (payload)

                let cyclic1_obj = ManagedObject {
                    id: ObjectId(0),
                    references: vec![cyclic2_id],
                    payload: vec![],
                };
                heap.write_object(&cyclic1_obj).unwrap();

                let cyclic2_obj = ManagedObject {
                    id: ObjectId(0),
                    references: vec![cyclic1_id],
                    payload: vec![],
                };
                heap.write_object(&cyclic2_obj).unwrap();

                // Возвращаем кортеж окружения, который пробросится в сам бенчмарк
                (gc, heap, vec![root_id])
            },
            |(mut gc, mut heap, roots)| {
                // Измеряем строго этот вызов
                gc.collect(&mut heap, &roots).unwrap();
            },
        );
    });
}

fn bench_gc_collect_big(c: &mut Criterion) {
    c.bench_function("gc_collect_with_large_allocated_space", |b| {
        b.iter_with_setup(
            || {
                // Создаем кучу побольше — например, 128 КБ
                let mut heap = ManagedHeap::new(128 * 1024);
                let gc = GarbageCollector::new();

                // 1. Создаем живой корень
                let root_obj = ManagedObject {
                    id: ObjectId(0),
                    references: vec![],
                    payload: vec![1, 2, 3, 4],
                };
                let root_id = heap.write_object(&root_obj).unwrap();

                // 2. Имитируем "историю" работы рантайма:
                // Забиваем кучу тысячей мелких мертвых объектов, двигая указатель `next` вперед.
                // У них refs_count изначально 0 и в корнях их нет.
                for _ in 0..1000 {
                    let dummy = ManagedObject {
                        id: ObjectId(0),
                        references: vec![],
                        payload: vec![0; 16], // 16 байт payload
                    };
                    heap.write_object(&dummy).unwrap();
                }

                (gc, heap, vec![root_id])
            },
            |(mut gc, mut heap, roots)| {
                // Вот теперь sweep будет вынужден прошагать через 1000 заголовков!
                gc.collect(&mut heap, &roots).unwrap();
            },
        );
    });
}

criterion_group!(benches, bench_gc_collect, bench_gc_collect_big);
criterion_main!(benches);