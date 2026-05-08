# OS-9 デバイスマップ雛形（MC6809・日本語）

本ドキュメントは MC6809 上の OS‑9 系セットアップ向けの推奨 I/O マッ
ピングと割込ベクタの目安をまとめたものです。実際のアドレスはターゲッ
トイメージ／ボード構成に合わせて調整してください。なお、本ファイルは
`docs/en/os9_device_map_templates.md`（canonical）の日本語版です。

> **注:** 後段のコマンド実行例 (`cargo run -- samples/...`) は本
> クレートをラップする GUI ホスト
> [em6809](https://github.com/hha0x617/em6809) を対象としたもので、
> trace / timer サンプルも em6809 側に同梱されています。em6809-core
> 自体はライブラリ専用ですが、I/O ページ・ベクタ・タイマに関する
> 設計指針は em6809 / [emfe MC6809 プラグイン](https://github.com/hha0x617/emfe_plugins/tree/master/mc6809)
> / 独自 embedder のいずれにも適用できます。

## I/O ページの提案
- コンソール（UART/ACIA）: ベース `0xFF00`
  - 制御／ステータス: `0xFF00`
  - データ RX/TX:     `0xFF01`
  - 任意の補助:       `0xFF02..0xFF03`
- タイマ: `0xFF10`
- ディスク／ブロック I/O: `0xFF20`
- 予約／空き: `0xFF30..0xFFEF`

注記:
- I/O ページは単純化のため `0xFF00..0xFFFF` の連続領域に置く。
- コンソールの IRQ/FIRQ 設定や RX ウォーターマーク設定はブートスクリ
  プトから行うとよい。

## 割込ベクタ（参考）
MC6809 のベクタはメモリ最上位に配置されます。ROM に応じて調整してく
ださい:
- FIRQ: 高速割込ハンドラ（例: 高頻度のコンソール RX）
- IRQ: 通常の割込ハンドラ（例: タイマ）
- NMI: 致命障害用のマスク不能割込
- RESET: システム開始

推奨:
- 初期化完了後はベクタ領域を書込保護する（ブートスクリプトの
  `attr` / `prot` などを利用）。
- 低レイテンシを優先する場合はコンソール RX を FIRQ に振り分け、それ
  以外は IRQ を使う。

## ブートスクリプトのヒント
- `on_step` / `on_pc` を用いて MMU マップ適用やデバイス IRQ 有効化を
  段階的に行う。
- `con_ctrl`、`con_rx_wm`、必要に応じて `con_irq_hold` を適用してコン
  ソールの挙動をチューニングする。
- `mode system` から `mode user` への切替はカーネルページがマップされ
  た後にのみ行う。

## タイマ（0xFF10）クイックリファレンス
- ベース: `0xFF10`
- レジスタ（ベースからのオフセット）:
  - `+0` CTRL/STATUS (R/W): bit0 RUN、bit1 IRQ_EN、bit2 FIRQ、
    bit3 PENDING (R)、bit4 PENDING を 1 書込でクリア
  - `+1..+2` RELOAD (R/W): 16 ビットのリロード周期（命令ティック数、
    ビッグエンディアン）
  - `+3..+4` COUNTER (R/W): 現在のダウンカウンタ
- IRQ ルーティング: `FIRQ` ビットが立っている場合は FIRQ に出力、それ
  以外は IRQ に出力する。

CLI 使用例（OS-9 カーネル不要）:
- 同梱の `trace_all.s19` などタイトループイメージにタイマを取り付け:
  - `cargo run -- samples/traces/trace_all.s19 --timer 0xFF10 --timer-reload 0x0010 --timer-irq --timer-start`
  - またはレート指定からリロード値を導出:
    `--timer-rate 1000 --timer-ips 1000000`

LED 点滅サンプル（約 1 Hz）:
- `samples/traces/timer_led.asm` をアセンブルして実行すると、タイマ
  ISR から GPIO の bit0 をトグルします。
- 推奨コマンド（FIRQ ルート、60 Hz ティック → 1 点滅／秒）:
  - `cargo run -- samples/traces/timer_led.s19 --gpio 0xFF30 --gpio-bits 8 --timer 0xFF10 --timer-rate 60 --timer-ips 1000000 --timer-irq --timer-firq --timer-start --run`

検証テスト:
- `tests/timer.rs` に IRQ/FIRQ ルーティングと `$FF10` の PENDING ビット
  クリアを検証する単体テストがあります。
