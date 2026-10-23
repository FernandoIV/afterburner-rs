// Enlazado al stack de GTK del sistema (sin crates externos: son libs
// compartidas del propio Linux con las que la app se ejecuta de todas formas).
fn main() {
    println!("cargo:rustc-link-lib=dylib=gtk-4");
    println!("cargo:rustc-link-lib=dylib=gobject-2.0");
    println!("cargo:rustc-link-lib=dylib=glib-2.0");
    println!("cargo:rustc-link-lib=dylib=gio-2.0");
    println!("cargo:rustc-link-lib=dylib=cairo");
}
