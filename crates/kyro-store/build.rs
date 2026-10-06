fn main() {
    // sqlx tracks existing include files; directory tracking also discovers newly added migrations.
    println!("cargo:rerun-if-changed=migrations");
}
