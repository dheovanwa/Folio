# Maintainer: Your Name <your.email@example.com>
pkgname=folio
pkgver=0.1.0
pkgrel=1
pkgdesc="A lightweight, distraction-free URL article reader"
arch=('x86_64')
url="https://github.com/user/folio"
license=('MIT')
depends=('gtk4' 'libadwaita' 'sqlite')
makedepends=('rust' 'cargo')
options=('!lto')
source=()
sha256sums=()

prepare() {
    export RUSTUP_TOOLCHAIN=stable
    cd "$startdir"
    cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target

    cd "$startdir"
    cargo build --frozen --release
}

package() {
    cd "$startdir"
    install -Dm755 "target/release/folio" "$pkgdir/usr/bin/folio"
    install -Dm644 "data/dev.personal.folio.desktop" "$pkgdir/usr/share/applications/dev.personal.folio.desktop"
    install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
