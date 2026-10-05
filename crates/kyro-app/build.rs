fn main() {
    // sqlx embeds migrations; new directory entries must also invalidate the binary.
    println!("cargo:rerun-if-changed=migrations");
}
