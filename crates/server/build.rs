// Сведения о продукте в Windows-исполняемом файле: видны в свойствах файла
// проводником, позволяют узнать версию, не запуская сервер.
fn main() {
    // Явные rerun-директивы (они отключают дефолтный трекинг каталога пакета):
    // build.rs и Cargo.toml (версия встраивается в exe) + env-переключатели
    // инструментов winresource. Без них смена RC_PATH не инвалидировала кэш, и
    // сборка залипала в «Fresh» без метаданных даже после починки окружения.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=RC_PATH");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=AR");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set("FileDescription", "BSL Context MCP Server");
    res.set("ProductName", "BSL Context");
    res.set("CompanyName", "regsorm-lab");
    res.set("LegalCopyright", "Copyright (C) 2026 regsorm-lab");
    res.set("OriginalFilename", "bsl-context-rs.exe");
    res.set("InternalName", "bsl-context-rs.exe");

    // Встраивание ресурса требует внешней утилиты: `rc.exe` из Windows SDK для
    // цепочки msvc, `windres` из mingw для цепочки gnu. Её отсутствие не должно
    // ронять сборку — сведения о версии косметические, а собрать проект нужно
    // всем, в том числе без установленного SDK.
    if let Err(e) = res.compile() {
        println!("cargo:warning=сведения о версии в exe не встроены: {e}");
    }
}
