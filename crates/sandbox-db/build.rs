//! `sqlx::migrate!` embeds the migrations at compile time; without this, adding a migration
//! file would not rebuild the crate and the binary would silently ship the old schema.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
