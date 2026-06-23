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
    let allocs_per_thread = 500_000;
    let total_allocs = num_threads * allocs_per_thread;

    // ----------------------------------------------------------------
    // ТЕСТ 4: Смешанный бенчмарк нашего Lock-Free аллокатора
    // ----------------------------------------------------------------
    println!("\n[Тест 4] Смешанный бенчмарк нашего Lock-Free аллокатора (Разные классы размеров)...");

    let concurrent_heap = Arc::new(ManagedHeap::new(6000 * 1024 * 1024)); // 600 MB
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

    bench_fragmentation_recovery();
    bench_gc_pause_scaling();
    bench_single_thread_baseline();
    bench_per_size_class();

    println!("\n🚀 Сравнение успешно завершено!");
}

// ============================================================
// БЕНЧМАРК A: Fragmentation Recovery
// Проверяет, реально ли занятая память (next_page_offset, через
// прокси get_page_map_len) перестаёт расти после второй волны
// аллокаций, если первая волна была наполовину собрана GC.
// ============================================================
fn bench_fragmentation_recovery() {
    println!("\n[Бенч A] Fragmentation Recovery — переиспользование памяти после GC...");

    let heap = Arc::new(ManagedHeap::new(64 * 1024 * 1024)); // 64 MB
    let central = Arc::new(MCentral::new());
    let mut cache = MCache::new(Arc::clone(&central), Arc::clone(&heap));
    let mut gc = GarbageCollector::new();

    let batch_size = 20_000usize;

    // --- Волна 1: создаём batch_size объектов, половину делаем мусором ---
    let mut roots: Vec<ObjectId> = Vec::new();
    let mut garbage: Vec<ObjectId> = Vec::new();

    for i in 0..batch_size {
        let obj = ManagedObject {
            type_desc_ptr: &LEAF_TYPE as *const _,
            payload: vec![0xAA; 56],
        };
        let id = cache.alloc(MSpanSizeClass::B64, &obj).unwrap();
        if i % 2 == 0 {
            roots.push(id);
        } else {
            garbage.push(id);
        }
    }

    let pages_after_wave1 = heap.get_page_map_len();
    println!("   После волны 1: записей в page_map: {}", pages_after_wave1);

    gc.initialize(&roots, &heap);
    gc.trace(&heap);
    gc.sweep(&heap, &central);

    for id in &garbage {
        assert!(!is_allocated(*id, &heap), "Мусор не был собран!");
    }
    for id in &roots {
        assert!(is_allocated(*id, &heap), "Живой объект погиб ошибочно!");
    }
    println!("   [OK] Волна 1 мусора корректно собрана.");

    // --- Волна 2: аллоцируем ещё столько же объектов того же класса ---
    let mut roots2: Vec<ObjectId> = Vec::new();
    for _ in 0..batch_size {
        let obj = ManagedObject {
            type_desc_ptr: &LEAF_TYPE as *const _,
            payload: vec![0xBB; 56],
        };
        let id = cache.alloc(MSpanSizeClass::B64, &obj).unwrap();
        roots2.push(id);
    }

    let pages_after_wave2 = heap.get_page_map_len();
    println!("   После волны 2: записей в page_map: {}", pages_after_wave2);

    // get_page_map_len — фиксированный размер вектора (heap_size / PAGE_SIZE + 1),
    // он не растёт. Метрика роста, которую реально можно наблюдать —
    // next_page_offset недоступен напрямую, поэтому считаем количество
    // занятых страниц (страниц с Some(span)) до и после.
    let occupied_after_wave1 = (0..pages_after_wave1)
        .filter(|&i| heap.get_span_for_page(i).is_some())
        .count();
    let occupied_after_wave2 = (0..pages_after_wave2)
        .filter(|&i| heap.get_span_for_page(i).is_some())
        .count();

    println!("   Занятых страниц после волны 1: {}", occupied_after_wave1);
    println!("   Занятых страниц после волны 2: {}", occupied_after_wave2);

    let growth = occupied_after_wave2.saturating_sub(occupied_after_wave1);
    println!("   Прирост занятых страниц во второй волне: {}", growth);

    // half of wave1 was garbage => роста почти не ожидаем, если recycling работает
    if growth == 0 {
        println!("   🎉 Память полностью переиспользована, новых страниц не выделено!");
    } else {
        println!("   ⚠️  Куча выросла на {} страниц несмотря на наличие свободных spans.", growth);
    }

    // не даём _roots2 быть warning'ом
    let _ = roots2.len();
}

// ============================================================
// БЕНЧМАРК B: GC pause time vs heap size (исправленная версия)
// ============================================================
fn bench_gc_pause_scaling() {
    println!("\n[Бенч B] GC Pause Time vs Heap Size...");

    let sizes = [10_000usize, 100_000, 500_000];

    for &n in &sizes {
        let heap = Arc::new(ManagedHeap::new((n * 128 + 4096).max(8 * 1024 * 1024)));
        let central = Arc::new(MCentral::new());
        let mut cache = MCache::new(Arc::clone(&central), Arc::clone(&heap));
        let mut gc = GarbageCollector::new();

        let mut roots = Vec::with_capacity(n);
        for _ in 0..n {
            let obj = ManagedObject {
                type_desc_ptr: &LEAF_TYPE as *const _,
                payload: vec![0xCC; 56],
            };
            roots.push(cache.alloc(MSpanSizeClass::B64, &obj).unwrap());
        }

        let start_init = Instant::now();
        gc.initialize(&roots, &heap);
        let init_time = start_init.elapsed();

        let start_trace = Instant::now();
        gc.trace(&heap);
        let trace_time = start_trace.elapsed();

        let start_sweep = Instant::now();
        gc.sweep(&heap, &central);
        let sweep_time = start_sweep.elapsed();

        println!(
            "   N={:>7} | initialize: {:>10?} | trace: {:>10?} | sweep: {:>10?} | итого: {:>10?}",
            n,
            init_time,
            trace_time,
            sweep_time,
            init_time + trace_time + sweep_time
        );

        // sanity check — все объекты должны остаться живыми (все они root)
        for id in &roots {
            assert!(is_allocated(*id, &heap), "Живой root ошибочно собран!");
        }
    }
}

// ============================================================
// БЕНЧМАРК C: Single-thread baseline
// Тот же смешанный патторн аллокации (60/30/10%), что в тестах
// 4/5, но в один поток — показывает, какую долю выигрыша даёт
// именно sharded-lock конкурентность, а не сама by-себе арифметика.
// ============================================================
fn bench_single_thread_baseline() {
    println!("\n[Бенч C] Single-thread baseline (наш аллокатор vs системный)...");

    let allocs = 2_000_000usize;

    // --- Наш аллокатор, один поток ---
    let heap = Arc::new(ManagedHeap::new(1500 * 1024 * 1024));
    let central = Arc::new(MCentral::new());
    let mut cache = MCache::new(Arc::clone(&central), Arc::clone(&heap));

    let start_custom = Instant::now();
    for i in 0..allocs {
        let (size_class, type_desc, payload_size) = if i % 10 == 0 {
            (MSpanSizeClass::B3072, &LARGE_TYPE, 3064)
        } else if i % 3 == 0 {
            (MSpanSizeClass::B512, &MEDIUM_TYPE, 504)
        } else {
            (MSpanSizeClass::B64, &SMALL_TYPE, 56)
        };
        let obj = ManagedObject {
            type_desc_ptr: type_desc as *const TypeDescriptor,
            payload: vec![0u8; payload_size],
        };
        cache.alloc(size_class, &obj).unwrap();
    }
    let elapsed_custom = start_custom.elapsed();

    // --- Системный аллокатор, один поток ---
    let start_sys = Instant::now();
    let mut heap_simulation = Vec::with_capacity(allocs);
    for i in 0..allocs {
        let block_size = if i % 10 == 0 {
            3072
        } else if i % 3 == 0 {
            512
        } else {
            64
        };
        let payload_size = block_size - 8;
        heap_simulation.push(vec![0u8; payload_size].into_boxed_slice());
    }
    let elapsed_sys = start_sys.elapsed();
    let _ = heap_simulation.len();

    println!("   Наш аллокатор (1 поток):      {:?}", elapsed_custom);
    println!("   Системный аллокатор (1 поток): {:?}", elapsed_sys);

    let speedup = elapsed_sys.as_secs_f64() / elapsed_custom.as_secs_f64();
    if speedup > 1.0 {
        println!("   🎉 В один поток наш быстрее в {:.2} раз — выигрыш не только от конкурентности.", speedup);
    } else {
        println!("   📈 В один поток системный быстрее в {:.2} раз — весь выигрыш в multi-thread от sharded locks.", 1.0 / speedup);
    }
}

// ============================================================
// БЕНЧМАРК D: Разбивка по size-классам
// Показывает, на каких именно классах размеров разница больше всего.
// ============================================================
fn bench_per_size_class() {
    println!("\n[Бенч D] Разбивка по size-классам (один поток, по 1M объектов каждого класса)...");

    let classes = [
        (MSpanSizeClass::B64, &SMALL_TYPE, 56usize, "B64"),
        (MSpanSizeClass::B512, &MEDIUM_TYPE, 504usize, "B512"),
        (MSpanSizeClass::B3072, &LARGE_TYPE, 3064usize, "B3072"),
    ];

    let allocs = 1_000_000usize;

    for (size_class, type_desc, payload_size, label) in classes {
        // ёмкость кучи с запасом под конкретный класс
        let heap_size = (allocs * size_class.block_size()) + 16 * 1024 * 1024;
        let heap = Arc::new(ManagedHeap::new(heap_size));
        let central = Arc::new(MCentral::new());
        let mut cache = MCache::new(Arc::clone(&central), Arc::clone(&heap));

        let start_custom = Instant::now();
        for _ in 0..allocs {
            let obj = ManagedObject {
                type_desc_ptr: &*type_desc as *const TypeDescriptor,
                payload: vec![0u8; payload_size],
            };
            cache.alloc(size_class, &obj).unwrap();
        }
        let elapsed_custom = start_custom.elapsed();

        let start_sys = Instant::now();
        let mut sim = Vec::with_capacity(allocs);
        for _ in 0..allocs {
            sim.push(vec![0u8; payload_size].into_boxed_slice());
        }
        let elapsed_sys = start_sys.elapsed();
        let _ = sim.len();

        let speedup = elapsed_sys.as_secs_f64() / elapsed_custom.as_secs_f64();
        println!(
            "   {:<6} | наш: {:>10?} | системный: {:>10?} | speedup: {:.2}x",
            label, elapsed_custom, elapsed_sys, speedup
        );
    }
}