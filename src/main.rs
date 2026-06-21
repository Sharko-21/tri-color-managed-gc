use std::sync::atomic::Ordering;
use std::sync::{Arc};
use std::thread;
use std::time::Instant;
use crate::datastruct::{ManagedObject, ObjectId, TypeDescriptor, PAGE_SIZE};
use crate::managed_heap::ManagedHeap;
use crate::mcache::MCache;
use crate::mcentral::MCentral;
use crate::mspan::MSpanSizeClass;
use crate::garbage_collector::GarbageCollector;

pub mod datastruct;
pub mod managed_heap;
pub mod mcache;
pub mod mcentral;
pub mod mspan;
pub mod garbage_collector;

// Дескрипторы для тестов аллокатора
static SMALL_TYPE: TypeDescriptor = TypeDescriptor { total_size: 64, refs_count: 0 };
static MEDIUM_TYPE: TypeDescriptor = TypeDescriptor { total_size: 512, refs_count: 0 };
static LARGE_TYPE: TypeDescriptor = TypeDescriptor { total_size: 3072, refs_count: 0 };

// Дескрипторы для тестов сборщика мусора
static LEAF_TYPE: TypeDescriptor = TypeDescriptor { total_size: 64, refs_count: 0 };
static NODE_TYPE: TypeDescriptor = TypeDescriptor { total_size: 64, refs_count: 1 };

/// Вспомогательная функция проверки: жив ли объект в битовой маске MSpan
fn is_allocated(id: ObjectId, heap: &ManagedHeap) -> bool {
    let page_num = id.0 / PAGE_SIZE;
    let span = heap.get_span_for_page(page_num).expect("Спан не найден!");
    let index = (id.0 - span.start_id.0) / span.size_class.block_size();
    let mask = span.alloc_bits[index / 64].load(Ordering::Acquire);
    (mask & (1 << (index % 64))) != 0
}

fn main() {
    println!("=== Запуск расширенного стресс-теста рантайма ===");

    let heap = Arc::new(ManagedHeap::new(1024 * 1024)); // 1 MB для базовых тестов
    let central = Arc::new(MCentral::new());
    let mut cache = MCache::new(Arc::clone(&central), Arc::clone(&heap));
    let mut gc = GarbageCollector::new();

    // ----------------------------------------------------------------
    // ТЕСТ 1: Кросс-классовая аллокация и валидация чтения
    // ----------------------------------------------------------------
    println!("\n[Тест 1] Аллокация объектов разных классов размеров...");

    let obj_small = ManagedObject {
        type_desc_ptr: &SMALL_TYPE as *const TypeDescriptor,
        payload: vec![0xAA, 0xBB, 0xCC, 0xDD],
    };
    let id_small = cache.alloc(MSpanSizeClass::B64, &obj_small).unwrap();

    let obj_medium = ManagedObject {
        type_desc_ptr: &MEDIUM_TYPE as *const TypeDescriptor,
        payload: vec![0x11, 0x22, 0x33, 0x44, 0x55],
    };
    let id_medium = cache.alloc(MSpanSizeClass::B512, &obj_medium).unwrap();

    let obj_large = ManagedObject {
        type_desc_ptr: &LARGE_TYPE as *const TypeDescriptor,
        payload: vec![0x99, 0x88, 0x77],
    };
    let id_large = cache.alloc(MSpanSizeClass::B3072, &obj_large).unwrap();

    let (_, data_small) = heap.read_from_slot(id_small, MSpanSizeClass::B64);
    assert_eq!(&data_small[0..4], &[0xAA, 0xBB, 0xCC, 0xDD]);

    let (_, data_medium) = heap.read_from_slot(id_medium, MSpanSizeClass::B512);
    assert_eq!(&data_medium[0..5], &[0x11, 0x22, 0x33, 0x44, 0x55]);

    let (_, data_large) = heap.read_from_slot(id_large, MSpanSizeClass::B3072);
    assert_eq!(&data_large[0..3], &[0x99, 0x88, 0x77]);

    println!("[OK] Данные успешно проверены.");

    // ----------------------------------------------------------------
    // ТЕСТ 2: Исчерпание спана и каскадный grow_span
    // ----------------------------------------------------------------
    println!("\n[Тест 2] Запуск массовой аллокации для исчерпания локального спана...");

    for i in 1..4 {
        let dummy_obj = ManagedObject {
            type_desc_ptr: &LARGE_TYPE as *const TypeDescriptor,
            payload: vec![i as u8; 4],
        };
        let id = cache.alloc(MSpanSizeClass::B3072, &dummy_obj).unwrap();
        println!("   Блок {} класса B3072 успешно выделен по адресу: {:?}", i, id);
    }

    println!("Выделяем 5-й блок (должен стриггерить выделение нового спана)...");
    let trigger_obj = ManagedObject {
        type_desc_ptr: &LARGE_TYPE as *const TypeDescriptor,
        payload: vec![0xFF; 4],
    };
    let id_new_span = cache.alloc(MSpanSizeClass::B3072, &trigger_obj).unwrap();
    println!("   5-й блок успешно выделен по адресу: {:?}", id_new_span);

    assert_eq!(
        id_new_span.0,
        id_large.0 + 12288,
        "Ошибка: Новый спан должен начинаться ровно на границе окончания старого!"
    );

    // ----------------------------------------------------------------
    // ТЕСТ 3: Сборка мусора (Mark & Sweep)
    // ----------------------------------------------------------------
    println!("\n[Тест 3] Инициализация графа объектов для GC...");

    let trash1_obj = ManagedObject {
        type_desc_ptr: &LEAF_TYPE as *const _,
        payload: vec![0xAA; 56],
    };
    let trash1_id = cache.alloc(MSpanSizeClass::B64, &trash1_obj).unwrap();

    let leaf_obj = ManagedObject {
        type_desc_ptr: &LEAF_TYPE as *const _,
        payload: vec![0xBB; 56],
    };
    let leaf_id = cache.alloc(MSpanSizeClass::B64, &leaf_obj).unwrap();

    let mut root_payload = vec![0x00; 56];
    root_payload[0..8].copy_from_slice(&leaf_id.0.to_ne_bytes());

    let root_obj = ManagedObject {
        type_desc_ptr: &NODE_TYPE as *const _,
        payload: root_payload,
    };
    let root_id = cache.alloc(MSpanSizeClass::B64, &root_obj).unwrap();

    let trash2_obj = ManagedObject {
        type_desc_ptr: &LEAF_TYPE as *const _,
        payload: vec![0xCC; 56],
    };
    let trash2_id = cache.alloc(MSpanSizeClass::B64, &trash2_obj).unwrap();

    println!("\n=== Запуск Garbage Collector ===");
    let roots = vec![root_id];
    gc.initialize(&roots, &heap);
    gc.trace(&heap);
    gc.sweep(&heap, &central);

    assert!(!is_allocated(trash1_id, &heap), "ОШИБКА: Мусор 1 выжил!");
    assert!(!is_allocated(trash2_id, &heap), "ОШИБКА: Мусор 2 выжил!");
    assert!(is_allocated(root_id, &heap), "ОШИБКА: Root погиб!");
    assert!(is_allocated(leaf_id, &heap), "ОШИБКА: Leaf погиб!");
    println!("[OK] Сборщик мусора отработал корректно.");

    // Общие параметры для новых бенчмарков производительности
    let num_threads = 8;
    // Снижаем количество аллокаций до 50 000, так как крупные объекты (3072) быстро съедают кучу.
    // 50 000 * 8 потоков = 400 000 разнородных объектов
    let allocs_per_thread = 50_000;
    let total_allocs = num_threads * allocs_per_thread;

    // ----------------------------------------------------------------
    // ТЕСТ 4: Смешанный бенчмарк нашего Lock-Free аллокатора
    // ----------------------------------------------------------------
    println!("\n[Тест 4] Смешанный бенчмарк нашего Lock-Free аллокатора (Разные классы размеров)...");

    let concurrent_heap = Arc::new(ManagedHeap::new(300 * 1024 * 1024)); // 300 MB
    let concurrent_central = Arc::new(MCentral::new());

    let mut handles = vec![];
    let start_custom = Instant::now();

    for t in 0..num_threads {
        let heap_clone = Arc::clone(&concurrent_heap);
        let central_clone = Arc::clone(&concurrent_central);

        handles.push(thread::spawn(move || {
            let mut local_cache = MCache::new(central_clone, heap_clone);

            for i in 0..allocs_per_thread {
                // Имитируем реальное распределение:
                // - 10% тяжелых объектов (3072 байт)
                // - 30% средних объектов (512 байт)
                // - 60% легких объектов (64 байта)
                let (size_class, type_desc, payload_size) = if i % 10 == 0 {
                    (MSpanSizeClass::B3072, &LARGE_TYPE, 3064)
                } else if i % 3 == 0 {
                    (MSpanSizeClass::B512, &MEDIUM_TYPE, 504)
                } else {
                    (MSpanSizeClass::B64, &SMALL_TYPE, 56)
                };

                let dummy_obj = ManagedObject {
                    type_desc_ptr: type_desc as *const TypeDescriptor,
                    payload: vec![t as u8; payload_size],
                };
                local_cache.alloc(size_class, &dummy_obj).unwrap();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let elapsed_custom = start_custom.elapsed();
    println!("   ⚡ Наш аллокатор выделил {} разнородных объектов за {:?}", total_allocs, elapsed_custom);

    // ----------------------------------------------------------------
    // ТЕСТ 5: Смешанный сравнительный бенчмарк системного аллокатора
    // ----------------------------------------------------------------
    println!("\n[Тест 5] Смешанный бенчмарк системного аллокатора Rust (std::alloc)...");

    let mut handles_sys = vec![];
    let start_sys = Instant::now();

    for t in 0..num_threads {
        handles_sys.push(thread::spawn(move || {
            let mut heap_simulation = Vec::with_capacity(allocs_per_thread);

            for i in 0..allocs_per_thread {
                // Точно такое же распределение размеров для кристальной честности теста:
                let block_size = if i % 10 == 0 {
                    3072
                } else if i % 3 == 0 {
                    512
                } else {
                    64
                };

                // Вычитаем 8 байт заголовка, чтобы имитировать размер payload
                let payload_size = block_size - 8;

                // Выделяем память в куче ОС
                let boxed_obj = vec![t as u8; payload_size].into_boxed_slice();
                heap_simulation.push(boxed_obj);
            }
            heap_simulation
        }));
    }

    for handle in handles_sys {
        let _retained_memory = handle.join().unwrap();
    }

    let elapsed_sys = start_sys.elapsed();
    println!("   ⚡ Системный аллокатор выделил {} разнородных объектов за {:?}", total_allocs, elapsed_sys);

    // ----------------------------------------------------------------
    // АНАЛИТИКА И СРАВНЕНИЕ
    // ----------------------------------------------------------------
    println!("\n=== Анализ производительности ===");
    let speedup = elapsed_sys.as_secs_f64() / elapsed_custom.as_secs_f64();
    if speedup > 1.0 {
        println!("🎉 Наш специализированный аллокатор БЫСТРЕЕ системного в {:.2} раз!", speedup);
    } else {
        println!("📈 Системный аллокатор оказался быстрее нашего в {:.2} раз.", 1.0 / speedup);
    }

    println!("\n🚀 Сравнение успешно завершено!");
}