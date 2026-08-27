# Zed / clangd による openFrameworks の C++ 静的解析メモ

Zed エディタで openFrameworks (oF) の C++ コードを快適に静的解析（コード補完、シンボルジャンプ、エラー診断）するための仕様・設定に関する技術メモ。

---

## 1. VSCode との相違点

VSCode (`ms-vscode.cpptools`) は `.vscode/c_cpp_properties.json` を使用し、`libs/openFrameworks/**` のような再帰的ワイルドカードパス展開をサポートしている。

一方、**Zed** はデフォルトで **`clangd`** (LLVM LSP) を使用する。`clangd` は VSCode 独自の設定やワイルドカード展開を解釈しないため、LLVM 標準の形式で設定を生成する必要がある。

| 項目 | VSCode (`cpptools`) | Zed (`clangd`) |
| :--- | :--- | :--- |
| **LSP** | Microsoft C/C++ IntelliSense | `clangd` (LLVM) |
| **設定ファイル** | `.vscode/c_cpp_properties.json` | `compile_commands.json` + `.clangd` |
| **フォーマット** | JSON (独自スキーマ) | JSON (LLVM Compilation Database) + YAML |
| **ワイルドカード (`/**`)** | サポート | **非サポート** (全パスを個別 `-I` で展開) |
| **ヘッダー単体の解決** | 自動 | `.clangd` の `CompileFlags` でフォールバック |

---

## 2. 生成する設定ファイル

### ① `compile_commands.json` (LLVM JSON Compilation Database)
各ソースファイル（`src/*.cpp`）ごとのコンパイル引数を定義する。

```json
[
  {
    "directory": "/path/to/myProject",
    "file": "src/main.cpp",
    "arguments": [
      "clang++",
      "-std=c++17",
      "-I/path/to/openFrameworks/libs/openFrameworks",
      "-I/path/to/openFrameworks/libs/openFrameworks/app",
      "-I/path/to/openFrameworks/libs/openFrameworks/graphics",
      "-I/path/to/openFrameworks/libs/glm/include",
      "-Isrc",
      "-c",
      "src/main.cpp"
    ]
  }
]
```

### ② `.clangd` (YAML 設定ファイル)
`compile_commands.json` の参照先と、ヘッダー単体（`ofApp.h` 等）を開いた際や新規ファイル用のフォールバック引数を定義する。

```yaml
CompileFlags:
  CompilationDatabase: .
  Add:
    - -std=c++17
    - -I/path/to/openFrameworks/libs/openFrameworks
    - -I/path/to/openFrameworks/libs/openFrameworks/app
    - -I/path/to/openFrameworks/libs/glm/include
    - -Isrc
```

※ macOS の場合は Xcode SDK の `-isysroot` や `-F.../System/Library/Frameworks` も追加する。

---

## 3. インクルードパスの収集ロジック

`clangd` はワイルドカードをサポートしないため、ジェネレータ側で以下のディレクトリをすべて再帰探索して個別の `-I` パスとして列挙する。

1. **プロジェクトソース**: `src/` 配下の全サブディレクトリ
2. **oF コア**: `libs/openFrameworks/` 配下の全サブディレクトリ (`app`, `graphics`, `gl`, `math`, `utils` など)
3. **外部ライブラリ**: `libs/*/include/` 配下の全サブディレクトリ (glm, boost, freetype, cairo など)
4. **アドオン (`addons.make`)**:
   - 各アドオンの `src/` 配下の全ディレクトリ
   - `libs/*/include/` 配下の全ディレクトリ
   - `addon_config.mk` に記述された OS 別除外ルール (`ADDON_SOURCES_EXCLUDE`, `ADDON_INCLUDES_EXCLUDE`) を適用
5. **システム SDK**:
   - macOS: Xcode SDK (`usr/include`, `System/Library/Frameworks`)
   - Windows: MSVC (`include`) および Windows SDK (`ucrt`, `shared`, `um`, `winrt`)

---

## 4. 参考リンク

- [Zed - C++ Language Support](https://zed.dev/docs/languages/cpp)
- [clangd - Configuration](https://clangd.llvm.org/config)
- [LLVM - JSON Compilation Database Format Specification](https://clang.llvm.org/docs/JSONCompilationDatabase.html)