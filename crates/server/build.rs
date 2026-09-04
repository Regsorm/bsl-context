fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set("FileDescription", "BSL Context MCP Server");
        res.set("ProductName", "BSL Context");
        res.set("CompanyName", "Regsorm");
        res.set("LegalCopyright", "Copyright (C) 2026 regsorm-lab");
        res.set("OriginalFilename", "bsl-context-rs.exe");
        res.set("InternalName", "bsl-context-rs.exe");
        res.compile().expect("failed to embed Windows resources for bsl-context-rs");
    }
}
