//! Модуль спана памяти (MSpan).
//!
//! Спан — это непрерывный участок виртуальной памяти (одна или несколько страниц по 4096 байт),
//! разбитый на блоки фиксированного размера. Каждый блок может хранить ровно один управляемый
//! объект. Состояние занятости блоков отслеживается с помощью битовых масок на атомарных `u64`.
//!
//! Спаны являются центральной единицей аллокации: `MCache` выделяет объекты из текущего спана,
//! `MCentral` управляет пулом пустых/частично заполненных спанов, а `GarbageCollector` использует
//! параллельную маску `marked_bits` для пометки живых объектов.

use strum_macros::EnumCount;
use crate::datastruct::{ObjectId};
use std::sync::atomic::{AtomicU64, Ordering};

/// Спан памяти — непрерывный регион, разделённый на блоки одного класса размера.
///
/// Каждый спан содержит:
/// - `alloc_bits` — битовая маска занятых блоков (для аллокатора).
/// - `marked_bits` — битовая маска живых блоков (заполняется GC во время mark-фазы).
///
/// Обе маски представляют собой массив из четырёх `AtomicU64`, что даёт максимум 256 блоков
/// на один спан. При создании спана проверяется, что количество блоков не превышает 256
/// (иначе assertion panic).
///
/// ## Потокобезопасность
///
/// Все операции с битовыми масками используют атомарные загрузки/сохранения с соответствующими
/// барьерами памяти (`Relaxed`, `Acquire`, `Release`). Это позволяет безопасно разделять `&MSpan`
/// между несколькими потоками (например, при чтении маски GC и записи аллокатором).
/// Сам `MSpan` не содержит `UnsafeCell` и может свободно разделяться через `Arc`.
pub struct MSpan {
    /// Класс размера блока, который определяет размер каждого элемента спана.
    pub size_class: MSpanSizeClass,
    /// Виртуальный адрес (смещение в `ManagedHeap`) первого блока спана.
    pub start_id: ObjectId,
    /// Общее количество блоков в спане (вычисляется при создании).
    pub total_blocks: usize,
    /// Атомарная битовая маска занятых блоков (4 слова по 64 бита = до 256 блоков).
    /// Бит установлен в 1 — блок занят, 0 — свободен.
    pub alloc_bits: [AtomicU64; 4],
    /// Атомарная битовая маска, заполняемая GC во время mark-фазы.
    /// После sweep-фазы эта маска сбрасывается в 0.
    pub marked_bits: [AtomicU64; 4],
}

impl MSpan {
    pub fn new(size_class: MSpanSizeClass, start_id: ObjectId) -> Self {
        let block_size = size_class.block_size();
        let total_pages = size_class.pages_per_span();
        let total_blocks = (total_pages * 4096) / block_size;
        assert!(total_blocks <= 256, "Спан вмещает больше 256 объектов, расширьте массив маски");
        const ZERO: AtomicU64 = AtomicU64::new(0);

        Self {
            size_class,
            start_id,
            total_blocks,
            alloc_bits: [ZERO; 4],
            marked_bits: [ZERO; 4],
        }
    }

    pub fn find_next_block(&self) -> Option<ObjectId> {
        let bits_to_check = self.total_blocks.div_ceil(64);
        for i in 0..bits_to_check {
            let current = self.alloc_bits[i].load(Ordering::Relaxed);
            if current == u64::MAX {
                break; // Свободных мест в этом слове нет, прерываем loop, идем к следующему i
            }
            let free_mask = !current;
            let bit_index = free_mask.trailing_zeros() as usize;
            let global_index = (i * 64) + bit_index;

            if global_index >= self.total_blocks {
                return None;
            }
            let block_size = self.size_class.block_size();
            let slot_offset = self.start_id.0 + (global_index * block_size);
            return Some(ObjectId(slot_offset));
        }
        None
    }
    pub fn publish_block(&self, object_id: ObjectId) {
        let global_index = (object_id.0 - self.start_id.0) / self.size_class.block_size();
        let mask_idx = global_index / 64;
        let bit_pos = global_index % 64;
        let mask = 1u64 << bit_pos;

        self.alloc_bits[mask_idx].fetch_or(mask, Ordering::Release);
    }
    /// Используется сборщиком мусора для сброса маски занятости объектов.
    /// Сборщик мусора составляет для каждого спана битовую маску живых и мертвых объектов
    /// В конце своего цикла мы делаем логическое И для каждого бита alloc_bits и зануляем
    /// маску marked_bits
    pub fn sweep(&self) {
        let bits_to_check = self.total_blocks.div_ceil(64);
        for i in 0..bits_to_check {
            let marked = self.marked_bits[i].load(Ordering::Acquire);

            // Оставляем занятыми только те слоты, которые GC пометил как живые
            self.alloc_bits[i].fetch_and(marked, Ordering::Release);

            // Обнуляем маркеры для следующего цикла сборки
            self.marked_bits[i].store(0, Ordering::Relaxed);
        }
    }

    pub fn is_full(&self) -> bool {
        // Считаем сумму всех установленных в 1 битов во всех 4-х u64
        let total_allocated: usize = self.alloc_bits
            .iter()
            .map(|word| word.load(Ordering::Acquire).count_ones() as usize)
            .sum();

        total_allocated >= self.total_blocks
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumCount)]
pub enum MSpanSizeClass {
    B64,
    B128,
    B256,
    B512,
    B3072,
}

impl MSpanSizeClass {
    pub fn block_size(&self) -> usize {
        match self {
            Self::B64 => 64,
            Self::B128 => 128,
            Self::B256 => 256,
            Self::B512 => 512,
            Self::B3072 => 3072,
        }
    }

    pub fn pages_per_span(&self) -> usize {
        match self {
            Self::B64 | Self::B128 | Self::B256 | Self::B512 => 1,
            Self::B3072 => 3, // Запрашиваем сразу 3 страницы (12288 байт) под 4 блока
        }
    }

    pub fn to_index(&self) -> usize {
        match self {
            Self::B64 => 0,
            Self::B128 => 1,
            Self::B256 => 2,
            Self::B512 => 3,
            Self::B3072 => 4,
        }
    }
}