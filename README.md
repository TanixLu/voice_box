<img src="assets/icon.svg" alt="VoiceBox" width="96">

# VoiceBox

VoiceBox是一个极简的麦克风降噪软件，提供单个exe文件，体积小巧，配置简单，运行高效。exe界面仅用于插件安装以及配置，APO插件安装完成后，关闭界面降噪仍旧生效。

## 构建

在 PowerShell 中运行 `./build.ps1`，发布构建使用 `./build.ps1 --release`。
可加 `--run` 在构建后运行程序，或加 `--skip-licenses` 跳过第三方声明更新。
声明生成需要先安装 `cargo install --locked cargo-about --features cli`。
构建产物位于 `target/debug` 或 `target/release`，包括 `voice_box.exe` 和 `voice_box_apo.dll`；EXE 已内嵌 APO DLL。

## 开源协议

本项目采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 协议，
第三方组件和模型声明见 [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES)，
也可在程序的“其他 → 版权声明”中查看。
