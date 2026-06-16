use crate::datastruct::{ManagedObject, ObjectId};
use crate::managed_heap::ManagedHeap;

pub mod datastruct;
pub mod allocator;
pub mod bump_allocator;
pub mod managed_heap;
pub mod garbage_collector;

fn main() {
    let mut heap = ManagedHeap::new(256);
    let obj1 = ManagedObject {
        id: ObjectId(0), // Аллокатор сам назначит правильный id, здесь пишем заглушку
        references: vec![ObjectId(10), ObjectId(20)], // 2 ссылки = 16 байт
        payload: vec![1, 2, 3, 4, 5, 6, 7, 8],        // 8 байт (уже выровнено)
    };
    let obj2 = ManagedObject {
        id: ObjectId(0), // Аллокатор сам назначит правильный id, здесь пишем заглушку
        references: vec![ObjectId(10), ObjectId(20)], // 2 ссылки = 16 байт
        payload: vec![9, 10, 11, 12],
    };
    let obj1_id = match heap.write_object(&obj1) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Ошибка записи объекта в кучу: {:?}", e);
            std::process::exit(1);
        }
    };
    let obj2_id = match heap.write_object(&obj2) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Ошибка записи объекта в кучу: {:?}", e);
            std::process::exit(1);
        }
    };
    print!("{:?}", obj1_id);
    print!("{:?}", obj2_id);
    let g_obj2 = match heap.read_object(obj2_id) {
        Ok(obj2) => obj2,
        Err(e) => {
            eprintln!("Ошибка получения объекта из кучи: {:?}", e);
            std::process::exit(1);
        }
    };
    print!("{:?}", g_obj2);
}
