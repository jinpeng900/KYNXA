# KYNXA 1.8.5

<p align="center">English | <a href="./README.zh.md">中文</a></p>

KYNXA is a personal agent assistant for Windows, bringing chat, files, tools, and ongoing work into one desktop application.

## What it can do

- Connect to cloud APIs or use supported local models.
- Work on multi-step tasks, accept guidance, and pause or resume saved work.
- Search project files and documents, with scoped memory and source references.
- Create and preview documents, spreadsheets, presentations, images, webpages, media, and supported 3D models.
- Use MCP services, application skills, browser tools, and local terminal operations.
- Keep files, webpages, and terminal sessions in the right workspace.
- Continue running in the system tray when the main window is hidden.

## Download

**Version 1.8.5 · Windows x64**

- [Download the EXE installer](https://github.com/jinpeng900/KYNXA/releases/download/v1.8.5/KYNXA-1.8.5-windows-x64-setup.exe)
- [Runtime resource pack](https://github.com/jinpeng900/KYNXA/releases/download/v1.8.5/KYNXA-1.8.5-windows-x64-resources.zip)
- [Release page and changes](https://github.com/jinpeng900/KYNXA/releases/tag/v1.8.5)

Run the EXE directly. The resource ZIP is for preparing a source build, not a standalone application.
A complete installation does not require development SDKs or manual PATH configuration. Cloud providers and external services may require your own account or credentials.

## Build from source

Use Windows x64 with Git, .NET 10 SDK (10.0.100 or a later 10.0 SDK), Rust 1.88+ with the MSVC toolchain, and Visual Studio C++ build tools with the Windows SDK.

```powershell
git clone https://github.com/jinpeng900/KYNXA.git
cd KYNXA
git checkout v1.8.5
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Run -Configuration Debug -Profile Full
```

Use `-Action Build` to build without launching. Existing caches are reused; offline builds also require prepared SDKs and Cargo/NuGet dependencies, plus a compatible resource pack supplied with `-Offline -ResourcePackRoot "path\resources.zip"`.

## License

[Apache License 2.0](LICENSE). Bundled third-party components retain their original licenses and notices.
