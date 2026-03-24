use std::env;

fn main() {
    dotenvy::dotenv().ok();
    println!("cargo:rerun-if-changed=.env");

    let key = "SQLX_OFFLINE";
    println!("cargo:rerun-if-env-changed={}", key);
    if let Ok(val) = env::var(key) {
        println!("cargo:rustc-env={}={}", key, val);
    }
}
