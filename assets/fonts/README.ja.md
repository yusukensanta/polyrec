[English](README.md) | 日本語

# 同梱フォント

PolyRec の UI 書体です。両方のファイルは `polyrec.exe` に組み込まれている（`src/ui/dashboard/theme.rs` の `include_bytes!`）ため、どのマシンでも UI は同じように表示されます。ユーザーがどのフォントをインストールしているかには依存しません。

| ファイル | ファミリー | アップストリームのリリース | SHA-256 |
|---|---|---|---|
| `IBMPlexSans-Regular.ttf` | IBM Plex Sans（ラテン文字） | [`@ibm/plex-sans@1.1.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans%401.1.0)、`packages/plex-sans/fonts/complete/ttf/` | `975dcda37d80f038dcd143c22e33ca2d97a0cc5a929aace1c749153b0fe1afa5` |
| `IBMPlexSansJP-Regular.ttf` | IBM Plex Sans JP（日本語） | [`@ibm/plex-sans-jp@3.0.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans-jp%403.0.0)、`packages/plex-sans-jp/fonts/complete/ttf/unhinted/` | `0168493ae143d16e4c0d374c090b1ee5eab9ee475536d2e429368dbcfc4a6abe` |

## ライセンス

どちらのフォントも © IBM Corp. で、Reserved Font Name「Plex」を伴う SIL Open Font License 1.1 の下でライセンスされています。全文は [`OFL.txt`](OFL.txt) にあります。このファイルは各パッケージの `LICENSE.txt` とバイト単位で同一です。OFL の要求に従い、リリースワークフローがこれを `LICENSE-IBM-Plex.txt` として zip とインストーラーに同梱しています。

## これらのファイルを選んでいる理由

- **未改変で、サブセット化もしていません。** Plex Sans JP をサブセット化すれば約 5.7 MB 削減できます。しかし OFL ではサブセットは「Modified Version」にあたり、Reserved Font Name により、それを「Plex」という名前で配布することは禁じられています。単体テスト `bundled_plex_fonts_are_the_unmodified_upstream_files` が上記のハッシュを固定しているため、誤ってファイルを変更すると CI が失敗します。
- **JP はヒントなし（unhinted）のビルド。** egui はアウトラインを自前でラスタライズし、TrueType のヒンティングを無視します。そのため、ヒント付きのビルドではバイト数が増えるだけです。
- **Regular ウェイトのみ。** egui には太字を合成する機能がありません。テーマでは強調を色とサイズで表現しているため、文字体系ごとに1つのウェイトで UI 全体をカバーできます。

## 更新手順

1. [IBM/plex](https://github.com/IBM/plex) の `@ibm/plex-sans@X` または `@ibm/plex-sans-jp@X` タグのリリースコミットから、新しい `.ttf` ファイルをダウンロードします。
2. このフォルダーのファイルを置き換えてから、上の表と、そのテスト内のハッシュを更新します。
3. `cargo test` を実行して、文字のカバー範囲を確認します。`src/i18n.rs` 内に、同梱フォントにグリフがない文字が1つでもあると、`bundled_fonts_cover_every_ui_string` が失敗します。
