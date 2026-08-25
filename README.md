# Animated2D Desktop Viewer

複数のゲームから抽出した 2D キャラクターアセットを、**元ゲームのランタイムに依存せず**再生する
デスクトップビューア／ランタイム。

対応予定のソースエコシステム:

- **Live2D Cubism**（Cubism 3+ を優先、Cubism 2 は後回し）
- **Spine**（複数の歴史的バージョン）
- 上記を Unity パッケージ化したゲーム固有の変種

インポータの初期対象は、Unity バンドルに同梱された Cubism モデル、`.skel.bytes` /
`.atlas.txt` 形式で配布される Spine スケルトン、Unity バンドルに同梱された Spine リグ
の 3 系統です。インポータは対象タイトル名ではなく、復元するアセットの形で命名します。

> **ステータス: Phase 1 完了（Spine 経路）／Phase 2 一部完了／Phase 6 実装済み**
> Spine アセットの検出・デコード・正規化・パッケージ化・アニメーション評価・GPU 描画・
> 透過デスクトップウィンドウが動作します。`animated2d preview` でビューアが開きます。
> Cubism / Unity 経路は未実装です。詳細は下の「実装状況」を参照してください。

---

## 設計の核

**Spine → Cubism への直接変換は行いません。** 双方を別々の内部 IR に正規化し、
上位の共通インターフェース（`AnimatedModel`）の裏に隠します。

```text
Game Asset Bundle / Extracted Files
        ↓  importers/   ゲーム固有の探索・再構成のみ
Source Format Detector
        ↓  formats/     バージョン別デコード → IR
Spine Decoder | Cubism Decoder
        ↓
Normalized Animated2D Model (IR)
        ↓  runtime/     決定論的なアニメーション評価
Runtime / Animator
        ↓  renderer/    ソース形式に非依存
Renderer
        ↓  desktop/
Desktop Host
```

守るべき不変条件は 2 つだけです。

1. **ゲーム固有の知識を `importers/` より下流に漏らさない**
2. **ソースバージョン固有の知識を `formats/` より下流に漏らさない**

レンダラがどのインポータ由来かを知る必要が出てきたら、設計が間違っています。

---

## クレート構成

Rust の cargo workspace。レンダリングは `wgpu`、デスクトップシェルは `winit` + `tray-icon`。

| クレート | 役割 |
| --- | --- |
| `a2d-core` | IR 型・数学・アニメーションデータモデル・エラー型・共通トレイト |
| `a2d-spine` | Spine バージョン検出、v2/v3/v4 デコーダ、Generic Spine IR への正規化 |
| `a2d-cubism` | moc3 / motion3 / physics / pose のデコードと正規化 |
| `a2d-unity` | Unity serialized file / AssetBundle のオブジェクトグラフ読み取り |
| `a2d-import` | generic / spine_bytes / unity_cubism / unity_spine の各インポータ |
| `a2d-pack` | `.a2dpack` の読み書き、マニフェスト、決定論的シリアライズ |
| `a2d-runtime` | スケルトン／パラメータ評価、タイムライン、コンストレイント、ミキシング |
| `a2d-render` | wgpu デバイス、テクスチャキャッシュ、バッチング、クリッピング |
| `a2d-desktop` | 透過ウィンドウ、ドラッグ、クリックスルー、トレイ、設定永続化 |
| `a2d-cli` | `animated2d` バイナリ |

依存方向は一方通行です。`a2d-render` と `a2d-runtime` は `a2d-import` / `a2d-unity` に
依存できません。逆向きに必要になった型は `a2d-core` へ移します。

---

## 内部パッケージ形式 `.a2dpack`

ビューアは生のゲームアセットを読みません。必ず正規化済みパッケージを読みます。

```text
character.a2dpack/
├─ manifest.json
├─ model.bin        # 正規化済み IR（ゲーム側のオブジェクトグラフではない）
├─ textures/
├─ animations/
└─ metadata/
```

```json
{
  "formatVersion": 1,
  "modelType": "spine",
  "sourceGame": "spine_bytes",
  "sourceFormat": "spine-3.8",
  "displayName": "CharacterName",
  "defaultAnimation": "idle",
  "textures": [],
  "animations": []
}
```

シリアライズは決定論的（フィールド順固定・マップはソート・浮動小数の書式固定）である必要が
あります。ゴールデンテストがこれに依存します。

---

## CLI

```bash
animated2d inspect  <input>                  # ゲーム/形式/バージョン/テクスチャ/アニメ名/非対応機能
animated2d import   <input> -o <out.a2dpack>
animated2d validate <package>
animated2d preview  <package> [--exit-after <秒>]   # ビューアで直接開く
```

`validate` のチェック項目: テクスチャ欠損 / 未解決アタッチメント / 非対応タイムライン /
不正なボーン親子 / 不正なスロット参照 / 壊れた atlas 参照 / 非対応コンストレイント。

---

## 実装状況

| クレート | 状態 |
| --- | --- |
| `a2d-core` | **実装済み** — IR、数学、`AnimatedModel`、`RenderMesh`、エラー分類、`LoadReport` |
| `a2d-spine` | **実装済み** — atlas パーサ、内容ベースのバージョン検出、JSON デコーダ（2.x/3.x/4.x 方言）、バイナリデコーダ（3.x / 4.0・4.1）、正規化 |
| `a2d-runtime` | **実装済み（Spine）** — ボーン変換（全 5 継承モード）、タイムライン評価、スキニング、deform、IK（1/2 ボーン）、transform コンストレイント（world / local × absolute / relative の 4 モード）、path コンストレイント（Tangent / Chain / ChainScale、4 種の spacing モード）、トラック／キュー／クロスフェード、idle ロジック |
| `a2d-pack` | **実装済み** — 決定論的な `model.bin`、`manifest.json`、`validate` |
| `a2d-import` | **実装済み（generic / spine_bytes / unity_cubism の調査まで）** — 内容ベースの分類、アセット探索、サフィックス正規化 |
| `a2d-cli` | **実装済み** — `inspect` / `import` / `validate` / `preview`（実描画・PNG 出力対応） |
| `a2d-render` | **実装済み** — wgpu 描画、テクスチャキャッシュ、バッチング、4 ブレンドモード、ステンシルクリッピング、high-DPI、透過背景、オフスクリーン描画と読み戻し |
| `a2d-desktop` | **実装済み** — 透過フレームレスウィンドウ、ドラッグ、スクロール拡縮、クリックスルー、最前面、トレイメニュー、モデル／アニメーション選択、設定永続化 |
| `a2d-unity` | **実装済み（読み取り）** — UnityFS コンテナ、serialized file、オブジェクト目録。実バンドルで検証済み |
| `a2d-cubism` | **一部実装** — MOC3 読み取り（ID / カウント / キャンバス / パラメータ範囲 / ドローアブルの UV・三角形）。独自パーサ方針（Cubism Core は使いません）。キーフォームと要素の対応付け、デフォーマ評価は未実装のため、まだポーズを作れません |

未対応の機能は黙って無視されるのではなく、`LoadReport` として `inspect` / `import` /
`validate` に必ず出力されます。既知の未対応: Spine 2.x バイナリ、Spine 4.2 バイナリ。
Spine のコンストレイントは IK / transform / path すべて実装済みです。

**4.0 / 4.1 バイナリデコーダは実物のエクスポートで未検証です。** テストは自前のライタとの
往復のみで、これはデコーダとライタが一致していることしか示しません（フィクスチャ自体が
デコーダと同じレイアウト理解で書かれているためです）。そのため、カウント値・文字列参照・
ファイル終端を厳しく検証し、レイアウトが違えば静かに壊れた結果を返すのではなく明確な
エラーで落ちるようにしてあります。読み込み時には毎回その旨を警告します。実物の
`.skel` を 1 つデコードして比較するまでは、4.x は JSON 形式の利用を推奨します。

```bash
animated2d preview character.a2dpack            # デスクトップビューアで開く
animated2d preview character.a2dpack -o frames/ # オフスクリーン描画して PNG 出力
```

`-o` なしでは透過ウィンドウが開きます。`-o` を付けると `0.0s / 0.25s / 0.5s / 1.0s` を
オフスクリーン描画し、ドローコール数・三角形数・フレームバッファのフィンガープリントを
表示しつつ各フレームを PNG として書き出します。GPU が無い環境ではエラーになります
（テストはスキップされます）。

```bash
animated2d preview character.a2dpack --exit-after 5   # 5 秒後に自動終了
```

`--exit-after <秒>` は終了操作（`Esc` / トレイ / 閉じるボタン）とまったく同じ経路で
ビューアを閉じます。スクリーンショットの自動撮影や、終了時に設定が本当に保存されるかを
確かめるスモークテストのための機能です。計測はウィンドウが開いてから始まります
（デバイス生成に数秒かかることがあるため）。終了時には提示したフレーム数を表示するので、
ウィンドウが実際に描画したかどうかを外から確認できます。

### ビューアの操作

| 操作 | 動作 |
| --- | --- |
| ドラッグ | キャラクターを移動 |
| スクロール | 拡大縮小 |
| `Space` | 一時停止／再開 |
| `Tab` | 次のアニメーション |
| `M` | 次のモデル |
| `T` | 最前面表示の切り替え |
| `C` | クリックスルーの切り替え |
| `F` | 左右反転 |
| `R` | 位置とサイズをリセット |
| `Esc` | 終了 |

すべてトレイメニューからも操作できます。クリックスルー中はウィンドウがマウスイベントを
受け取らないため、トレイが唯一の操作手段になります。

ウィンドウ位置・サイズ・選択中のモデルとアニメーション・拡大率は終了時に保存され、
次回起動時に復元されます（Windows なら `%APPDATA%\animated2d\config.json`、macOS なら
`~/Library/Application Support/animated2d/config.json`、それ以外は
`$XDG_CONFIG_HOME/animated2d/config.json`）。環境変数 `A2D_CONFIG_DIR` を
設定するとその場所を上書きできます。

## 開発環境

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

初期ターゲットプラットフォームは **Windows** です。WSL2 側でロジック・パーサ・
ゴールデンテストを回し、ウィンドウ／トレイ／透過まわりは Windows ネイティブで確認する
運用を想定しています。

---

## テスト方針

| 種別 | 内容 |
| --- | --- |
| ユニット | バイナリパーサ、バージョン検出、ボーン変換、ウェイトスキニング、補間、ベジェ、描画順、クリッピング、atlas パース、命名正規化 |
| ゴールデン | `ソースアセット → インポータ → IR → 決定論的シリアライズ` をフィクスチャと比較 |
| ビジュアル回帰 | `0.0s / 0.25s / 0.5s / 1.0s` の固定タイムスタンプで描画し、画像またはフレームバッファハッシュを許容誤差付きで比較 |
| 実装間検証 | 公式 Spine / Cubism ランタイムや既存ビューアと同一タイムスタンプの見た目を比較 |

微細な変形リグレッションを検出できるのはビジュアル回帰テストだけです。

**アセットの取り扱い:** 抽出したゲームアセットと proprietary SDK バイナリはコミットしません。
実アセットは gitignore された `tests/fixtures/local/` に置き、リポジトリには合成／手書きの
最小モデルと IR スナップショットのみを置きます。

---

## ロードマップ

| Phase | 内容 | ゴール |
| --- | --- | --- |
| 1 | atlas パーサ、実ターゲット 1 バージョンの skeleton デコーダ、ボーン／スロット／region・mesh・weighted mesh、基本タイムライン、GPU 描画 | Spine キャラを 1 体正しく表示 |
| 2 | deform / draw order / color / clipping / IK / transform constraint / ミキシング | 複数キャラの idle が正しく再生される |
| 3 | Unity バンドル調査、MOC3 抽出、Texture2D 抽出、AnimationClip 復元、Cubism パッケージ生成 | サンプルの Cubism バンドルを表示し idle を再生 |
| 4 | `GenericCubismModel : AnimatedModel` をビューアに統合 | 1 つのビューアで 2 系統のランタイム |
| 5 | unity_spine インポータ（立ち絵のみ） | Unity 同梱の Spine キャラの idle を Generic Spine Runtime で表示 |
| 6 | 透過ウィンドウ、ドラッグ、クリックスルー、トレイ、idle ロジック、設定永続化 | デスクトップマスコット化 |

### MVP の完了条件

1. `.skel.bytes` 形式の Spine キャラをインポートして表示できる
2. Unity 同梱の Cubism モデルをインポートして表示できる
3. 両方で idle アニメーションが動く
4. 同一のデスクトップビューア UI から両方を開ける
5. レンダラにゲーム固有の分岐が 1 つも無い
6. ビューアが生アセットではなく正規化済みパッケージを読む
7. パーサ／ランタイムの自動テストが通る
8. 各ランタイムファミリに最低 1 つビジュアル回帰テストがある

---

## やらないこと（初期バージョン）

- Spine Editor / Cubism Editor との完全互換
- モデルの編集・オーサリング
- 独自形式から proprietary な Spine/Cubism プロジェクト形式への書き戻し
- 戦闘固有の挙動、ゲームサーバのエミュレーション
- Spine → Cubism の直接変換
- 3D レンダリング
- 基本表示ができる前に元エンジンの全機能を再現すること

---

## 未決定事項

- **Cubism Core の扱い**: 公式 Cubism Core はライセンス条件のある proprietary ネイティブ
  ライブラリです。独自 MOC3 パーサを書くかどうかで `a2d-cubism` の設計が変わるため、
  MOC3 まわりの実装前に方針を決める必要があります。
- **デスクトップシェル**: 既定案は `winit` + `wgpu` + `tray-icon`（透過・クリックスルー・
  最前面すべて可能で、wgpu サーフェスを直接持てる）。Next.js 製のコントロールパネルが
  欲しい場合は Tauri v2 が代案ですが、WebView 下での wgpu 合成に手間がかかります。
- **Unity デシリアライズ**: 既存クレートを使うか、`a2d-unity` に最小実装を書くか。
  実バンドルで検証してから決めます。

---

## ライセンス

本リポジトリの**ソースコード**は、以下のいずれかのライセンスの下で提供されます。
利用者はどちらかを選択できます。

- Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) または <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License
  ([LICENSE-MIT](LICENSE-MIT) または <http://opensource.org/licenses/MIT>)

Rust エコシステムの慣例に従ったデュアルライセンスです。各クレートの `Cargo.toml` にも
`license = "MIT OR Apache-2.0"` として記載しています（`publish = false` のため
crates.io へは公開されません）。トップレベルの [LICENSE](LICENSE) は両者への案内であり、
実際の条文は上記 2 ファイルです。

このライセンスが適用されるのは本リポジトリのコードのみです。ゲームアセットおよび
proprietary SDK バイナリはリポジトリに含まれておらず、それらの権利は各権利者に帰属します。

### コントリビューション

明示的に別段の定めをしない限り、Apache-2.0 ライセンス第 5 条の定義に従い、
本プロジェクトへ意図的に提出されたコントリビューションは上記のデュアルライセンスの下で
ライセンスされ、追加の条件は付されません。

---

## 免責

本プロジェクトは、利用者自身が所有するソフトウェアから自分で抽出したアセットを、
個人的にオフラインで閲覧するためのものです。DRM の回避、ライセンスチェックの迂回、
ゲームサーバとの通信、アカウント操作の自動化は実装しません。
ゲームアセットおよび proprietary SDK バイナリの再配布も行いません。
