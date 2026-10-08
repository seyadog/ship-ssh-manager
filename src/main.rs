mod secrets;
mod store;

fn main() {
    let id = 999_999;
    println!("set: {:?}", secrets::set(id, "prueba"));
    println!("get: {:?}", secrets::get(id));
    secrets::delete(id);
    println!("after delete: {:?}", secrets::get(id));
}
