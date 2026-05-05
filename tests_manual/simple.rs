extern "C" {
    fn valo_checkpoint();
    fn valo_restore();
}

fn main() {
    for i in 0..15 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        println!("{i}");
        if i == 5 {
            unsafe { valo_checkpoint() };
        }
        if i == 10 {
            unsafe { valo_restore() };
        }
    }
}
