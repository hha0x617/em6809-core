# モジュール概要

各モジュールの `cargo doc` ページ (ファイル冒頭の `//!` ブロック) が
**正規の**リファレンスです。本ドキュメントは GitHub 上で読みやすい
形にしたミラーで、ローカルに clone して `cargo doc` を生成しなくても
全体を見渡せるようにしています。

## `bus` — アドレスバス抽象化

CPU が外界へアクセスする際の `Bus` トレイトと、すぐ使える 2 種類の
実装を提供します。周辺デバイス対応のバスが必要な場合は `io` モジュール
の `io::IoBus` を上に重ねます。

- `trait Bus { fn read8(&mut self, u16) -> u8; fn write8(...); ... }`
  — `read8_fetch` (実行権限と読み出しを区別) や
  `irq_lines() -> (irq, firq, nmi)` 等の拡張点あり。
- `Memory` — フラット 64 KiB の `[u8; 0x10000]`。ヘルパ:
  `clear(value)`, `load_slice(base, &[u8])`,
  `read_slice(start, len) -> &[u8]`。
- `WriteTrack` — 任意の `Box<dyn Bus>` をラップし、指定アドレス範囲
  内の書き込みを記録。em6809 では自己書換コード領域の再ディスアセンブル
  に利用。ヘルパ: `set_span(...)`, `take_dirty_addrs() -> Vec<u16>`,
  `inner_any_mut()`。

```rust
use em6809_core::bus::{Bus, Memory};

let mut bus = Memory::new();
bus.load_slice(0x0100, &[0x12, 0x12, 0x39]); // NOP NOP RTS
assert_eq!(bus.read8(0x0102), 0x39);
```

## `cpu` — MC6809 CPU とレジスタ

実装者が最も触る中心モジュール。CPU 状態と命令単位の step ルーチンを
持ちます。

- `Registers` — `a`, `b`, `x`, `y`, `u`, `s`, `pc`, `dp`, `cc`。
  `Copy + Clone + Default` で値スナップショット可能。
- `Cpu` — `cpu.r: Registers`, `cpu.cycles: u64`, 埋込の
  `debug::ShadowCallStack`, `nmi_pending` / `firq_pending` /
  `irq_pending` ラッチを保持。主なメソッド:
  - `Cpu::new()` — オールゼロの新規 CPU。
  - `Cpu::reset(&mut bus)` — `$FFFE/F` のリセットベクタから PC をロード。
  - `Cpu::set_pc(u16)` — 任意アドレスから開始 (`--pc` CLI フラグ用)。
  - `Cpu::step(&mut bus, trace) -> u32` — 1 命令実行、消費サイクル数を返す。
  - `Cpu::step_over(...)` / `Cpu::step_out(...)` — デバッガ用プリミティブ。
    `StepStop` で停止理由を返す。
  - `Cpu::request_nmi()` / `request_firq()` / `request_irq()` —
    割込ラッチを立てる。次の `step()` でサービス。
- `enum StepStop` — `ReturnTarget`, `Breakpoint(BreakpointId)`,
  `Limit`, `NotACall`, `EmptyStack`。
- 自由関数: `set_irq_log(bool)` (IRQ トレースログのグローバルトグル),
  `regs_snapshot(&Cpu) -> Registers` (UI 用にレジスタを clone)。

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;

let mut bus = Memory::new();
let mut cpu = Cpu::new();
cpu.reset(&mut bus);              // PC <- $FFFE/F のベクタ
let cycles = cpu.step(&mut bus, /* trace = */ false);
println!("first instruction took {cycles} cycles");
```

## `loader` — イメージパーサとバスローダ

プログラムイメージを構造化結果として返すか、バス/メモリへ直接書き込みます。

- `enum ImageFormat { Binary, Srec }` — GUI の `--format` フラグと対応。
- `struct ParsedImage { blocks: Vec<(u16, Vec<u8>)>, loaded_ranges,
  entry: Option<u16> }` — 完全なパース結果。
- `struct LoadedImage { loaded_ranges, entry: Option<u16> }` —
  ロード後のサマリ (バイト本体は持たない)。
- `parse_binary(base, &[u8]) -> ParsedImage` — 単一ブロックのラップ。
- `parse_srec(&str) -> Result<ParsedImage, String>` — Motorola
  S-Record パーサ。S0/S1/S2/S3/S7/S8/S9 レコードに対応。
- `load_binary(...)` / `load_binary_bus(...)` / `load_srec(...)` /
  `load_srec_bus(...)` — `parse_*` してから `Memory` または任意の
  `Bus` に書込。

```rust
use em6809_core::bus::Memory;
use em6809_core::loader::load_srec;

let srec = std::fs::read_to_string("hello.s19").unwrap();
let mut mem = Memory::new();
let img = load_srec(&mut mem, &srec).expect("valid S-Record");
if let Some(entry) = img.entry {
    println!("entry point: ${:04X}", entry);
}
```

ペリフェラル書込 (ACIA/GPIO 等) を尊重する形でロードしたい場合は
`io::IoBus` の上で `_bus` 変種を使ってください。

## `disasm` — 単命令およびウィンドウディスアセンブラ

`Bus` からバイトを読み、ニーモニック文字列に変換します。em6809 GUI の
リスティングペイン描画と統合テストの「この PC はこの命令と解釈される」
検証用の正規ルートとして機能します。

- `disasm_one(bus, pc) -> (u16, String)` — `(byte_length,
  "MNEMONIC OPERAND")`。`pc` を `byte_length` 進めると次の命令。
- `disasm_one_hex(bus, pc) -> (u16, String)` — 同上、ただし文字列の
  先頭に生バイト (`"$1F $89 ..."`) を付加 (16 進ダンプビュー用)。
- `disasm_window(bus, pc, before, after) -> Vec<DisasmLine>` —
  `pc` 周辺の命令帯。既知の命令境界にアンカーするため、複数バイト命令
  の途中に着地しない (リスティングのスクロールで重要)。
- `type DisasmLine = (u16, String)` — `(address, mnemonic_text)`。

```rust
use em6809_core::bus::Memory;
use em6809_core::disasm::disasm_one;

let mut bus = Memory::new();
bus.load_slice(0x0100, &[0x12]);  // NOP
let (len, text) = disasm_one(&mut bus, 0x0100);
assert_eq!((len, text.as_str()), (1, "NOP"));
```

## `io` — ペリフェラルとデバイス対応バス

プレーンな `Bus` (または MMU 配下のバス) をメモリマップド・デバイス
リストでラップし、ペリフェラルアドレス範囲への読書はデバイス側に
ディスパッチします。em6809 / emfe_plugin_mc6809 が CPU に渡す実バスです。

- `trait Device` — `contains(addr)`, `read8/write8`, オプションの
  `irq_lines() -> (irq, firq, nmi)` を実装。
- `Mc6850Dev` — Motorola **MC6850 ACIA** 互換 UART (`+0` SR/CR,
  `+1` RDR/TDR)。ヘルパ: `feed_bytes(&[u8])`, 出力 tee
  (`set_out_file`, `set_tee_stderr`, `set_flush_*`, `set_local_echo`),
  IRQ/FIRQ 配線 (`set_irq_hold_cycles`, `set_firq`)。Hha Forth/Hha Lisp,
  NetBSD MVME147 ブート ROM が利用。
- `BlockDev` — セクタアドレス可能なディスク。バック保管はホストファイル
  (`set_backing_file`) または in-memory イメージ (`set_image`)。
  `last_cmd()`, `last_data()`, `status()`, `take_dirty()` を公開。
- `GpioDev` — 汎用メモリマップド GPIO (`get_state() -> (out, dir,
  value)`)。
- `IoBus<B>` — デバイス対応バス本体。`inner: B` と
  `Vec<Box<dyn Device>>` を保持。
  - `IoBus::new(inner)` / `add_device(dev)`。
  - `ensure_console` / `ensure_block` / `ensure_gpio` / `ensure_timer`
    — 設定フラグから標準ペリフェラルを設置/撤去。
  - `with_console_mut` / `with_block_mut` / `with_gpio_mut` /
    `with_timer_mut` — クロージャ内でデバイスへ可変アクセス。
  - `feed_console_input(bytes)` — 一般用途のショートカット。
- 自由関数で GUI 連携: `set_console_log`, `set_console_gui_*`,
  `take_console_gui_bytes`, `set_console_repaint_callback`,
  `publish_gpio_broadcast`, `take_gpio_broadcast`,
  `peek_gpio_broadcast`。

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::io::IoBus;

// プレーン Memory + コンソール ($FF00) + 小型ブロックディスク ($FF10)
let mut bus = IoBus::new(Memory::new());
bus.ensure_console(true, 0xFF00);
bus.ensure_block(true, 0xFF10);

let mut cpu = Cpu::new();
cpu.reset(&mut bus);
for _ in 0..1_000 {
    cpu.step(&mut bus, /* trace = */ false);
}
```

## `mmu` — MC6829 ページ MMU

Motorola **MC6829** ページ MMU。NetBSD/MVME147 で使用。論理 16 ページ
× 4 KiB = 64 KiB CPU アドレス空間で、各ページを 16 ビット物理フレーム
にマップ。最大 8 タスクコンテキスト、ページ毎の W/R/X 属性、設定可能な
レジスタウィンドウ。

- `Mc6829` — `Bus` を実装するため、`Bus` を期待する任意の場所に投入可能。
  メソッド:
  - `Mc6829::new(phys_bytes, regs_base)` — 新規 MMU。`regs_base` は
    設定ウィンドウの論理アドレス。
  - `identity_map_current()` — バイパスモード: 現タスクで論理 N → 物理 N。
    OS がマップを書き換えるまでの実機ブート時のデフォルト。
  - `set_task(t: u8)`, `set_map_entry(page, frame)` — アクティブタスク
    のマップを直接更新。
  - `snapshot_current_map()`, `snapshot_map_for(sys_mode)`,
    `snapshot_maps()` — UI/デバッガ表示用にマップを取得。
  - `store_logical_slice(base, &[u8])` /
    `store_physical_slice(pbase, &[u8])` /
    `clear_physical(value)` — 論理/物理アドレスへイメージデータをロード。
  - `set_log_maps(bool)` — 翻訳ログを冗長化。

DSL (`task N`, `map page=frame`, `attr ...`, `prot ...`) による設定は
`config` モジュールに、トリガベース (`OnPc`, `OnStep`) のブート時設定は
`bootscript` モジュールにあります。

```rust
use em6809_core::cpu::Cpu;
use em6809_core::mmu::Mc6829;

// 物理 64 KiB、レジスタウィンドウ $FFE0 (論理)
let mut mmu = Mc6829::new(0x10000, 0xFFE0);
mmu.identity_map_current();
mmu.store_logical_slice(0x0100, &[0x12, 0x12, 0x39]); // NOP NOP RTS

let mut cpu = Cpu::new();
cpu.set_pc(0x0100);
cpu.step(&mut mmu, /* trace = */ false);
```

## `timer` — 最小周期タイマデバイス

メモリマップドのカウントダウンタイマ。周期割込を生成。`Device` を
実装するため `IoBus` に直接プラグイン可能。

- レジスタレイアウト: `+0` CTRL/STATUS (`RUN`, `IRQ_EN`, `FIRQ`,
  `PENDING`), `+1..+2` `RELOAD` (命令単位の 16 ビット周期),
  `+3..+4` `COUNTER`。
- `TimerDev` — メソッド:
  - `TimerDev::new(base)` — 停止状態で生成。
  - `set_reload(u16)` / `start()` / `stop()` — レジスタ書込を介さず
    プログラム制御 (テスト用)。
  - `set_irq_enable(bool)` / `set_firq(bool)` — CTRL を直接触らずに
    IRQ/FIRQ 配線。
  - `get_state() -> (run, irq_en, firq, pending)` — UI スナップショット。
  - `get_info() -> (reload, counter)` — UI スナップショット。

```rust
use em6809_core::bus::Memory;
use em6809_core::io::IoBus;
use em6809_core::timer::TimerDev;

let mut bus = IoBus::new(Memory::new());
let mut t = TimerDev::new(0xFF20);
t.set_reload(10_000);
t.set_irq_enable(true);
t.start();
bus.add_device(t);
```

設定フラグ駆動の自動設置/撤去には `IoBus::ensure_timer` を使うと
シンプルです。

## `debug` — デバッガプリミティブ

CPU 自身に持たせない GUI デバッガ機能の集合: ブレークポイント
(条件式評価器付き)、シャドウコールスタック、命令境界トラッキング、
小さなメモリ/レジスタダンプヘルパ。`Cpu` は `ShadowCallStack` を
埋め込み、毎ステップ `BreakpointSet` を参照するため、実装者は
これらを設定するだけで CPU が記録を行います。

**ブレークポイント**

- `BreakpointId` — `u32` の opaque newtype。`BreakpointSet::add()` の
  戻り値で、以降の操作に使用。
- `Breakpoint` — `pub address: u16`, `pub enabled: bool`,
  `pub condition: Option<String>`, `pub hit_count: u64`,
  `pub ignore_count: u64`。
- `BreakpointSet` — `Vec<Breakpoint>` を保持。メソッド:
  - `add(addr) -> BreakpointId` / `remove(id)` /
    `set_enabled(id, bool)` / `set_condition(id, Option<String>)`。
  - `should_break(pc)` — 高速な事前チェック。
  - `check(pc, &Registers)` — 条件式評価込みのフルチェック
    (`should_break` が `Some` を返した時に呼ぶ)。
  - `iter()` / `len()` (UI 描画用)。
  - 条件式言語: `==`, `!=`, `<`, `<=`, `>`, `>=`, `&&`, `||`, `!`,
    `+`, `-`, `*`, `&`, `|`, `^`, 括弧, 16 進 (`0x..` / `$..`),
    10 進、レジスタ名 (`a`/`b`/`d`/`x`/`y`/`u`/`s`/`pc`/`dp`/`cc`)
    に対応。

**シャドウコールスタック**

- `CallKind` — フレームを push したのは何か (`Bsr` / `Lbsr` / `Jsr`
  / `Swi` / `Irq` / `Firq` / `Nmi`)。
- `CallFrame` — `pub return_addr: u16`, `pub kind: CallKind` と、
  UI 表示用のレジスタスナップショットを少々。
- `ShadowCallStack` — append-only な `Vec<CallFrame>`。`frames()`,
  `top()`, `depth()` で読出。push/pop は CPU が直接行う。

**命令境界**

- `InstructionBoundaries` — 命令境界開始と判明したアドレス範囲の集合。
  GUI のリスティングペインが複数バイト命令の途中に着地しないように。
- `linear_sweep(...)` — 任意アドレス範囲を線形に walk して
  `InstructionBoundaries` を構築。

**自由ダンプヘルパ**

- `dump_registers(&cpu)` — stdout に出力 (CLI/テスト用)。
- `dump_memory(&mem, start, len)` /
  `dump_memory_bus(&mut bus, ...)` /
  `dump_memory_ascii(&mut bus, ...)` — `&Memory` または
  `&mut Bus` の hex / hex+ASCII ダンプ。

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::debug::BreakpointSet;

let mut bus = Memory::new();
let mut cpu = Cpu::new();
let mut bps = BreakpointSet::default();
let id = bps.add(0x1234);
bps.set_condition(id, Some("a == 0x42 && pc < $2000".into()));

cpu.reset(&mut bus);
loop {
    if let Some(hit) = bps.check(cpu.r.pc, &cpu.r) {
        println!("stopped on bp {:?}", hit);
        break;
    }
    cpu.step(&mut bus, false);
}
```

## `config` — MMU 設定 DSL

`Mc6829` を起動時に設定するためのテキスト形式 DSL。em6809 GUI の
`--mmu-config <file>` フラグや、レジスタを 1 つずつ手動で書きたく
ない統合テストで使用。

**文法** — トークンは空白区切り、1 行 1 ステートメント。`#` と `//`
が行コメント開始記号。数値は 16 進 (`0x..` / `$..`) または 10 進。

| ステートメント | 効果 |
|---|---|
| `task N` | アクティブタスクを `N` (0..7) に切替 |
| `mode sys` / `mode user` | CPU モードビット切替 |
| `map P=F` | 論理ページ `P` を物理フレーム `F` にマップ |
| `attr P=B` | 論理ページ `P` の属性バイト設定 (bit0 WPROT, bit1 RPROT, bit2 NX) |
| `prot N` | 保護制御バイト書込 |
| `regs <addr>` / `regs off` | レジスタウィンドウの移動/無効化 |

**提供関数**

- `apply_mmu_config_from_str(&mut Mc6829, &str) -> Result<(), String>`
  — パース + 適用。パース失敗時は `Err(line N: ...)` を返すので、
  呼出側はユーザに位置を提示できる。
- `apply_preset(&mut Mc6829, &str) -> Result<(), String>` — 名前付き
  プリセット適用 (`identity`, `netbsd_mvme147` 等)。設定ファイル
  なしで既知の良好状態を得たい時に便利。
- `list_presets() -> &'static [&'static str]` — プリセット名一覧。
  GUI のプリセットドロップダウンを駆動。

```rust
use em6809_core::mmu::Mc6829;
use em6809_core::config::{apply_mmu_config_from_str, apply_preset};

let mut mmu = Mc6829::new(0x10000, 0xFFE0);
apply_preset(&mut mmu, "identity").unwrap();

let cfg = "
    task 0
    map 0x0=0x0000  // 論理 $0xxx → 物理フレーム 0
    map 0x1=0x0001
    attr 0xF=0x01   // $Fxxx を WPROT
";
apply_mmu_config_from_str(&mut mmu, cfg).unwrap();
```

## `bootscript` — トリガ駆動ブートスクリプト DSL

エミュレーション中の「Y が起きたら X する」を記述する小型 DSL。
em6809 の `--boot-script` CLI オプションで MMU マッピング、
コンソール/ブロック/タイマデバイス状態、CPU 割込マスク状態をブート
シーケンスの特定 PC または step カウントで設定するのに使用。同 DSL
を使いたい任意の embedder で再利用可能。

**文法** — 各行が `<trigger>: <action>`。トリガ:

- `at_pc <addr>` — `cpu.r.pc == addr` 時に発火。
- `at_step <N>` — グローバル命令カウントが `N` に達した時に発火。

アクションはブート時の典型設定項目をカバー (`Action` enum 全項目を
参照)。コメントは `#` または `//` で開始、空行は無視。

**提供型/関数**

- `enum Action` — `Mode(bool)`, `Prot(u8)`, `Map(usize, u16)`,
  `Attr(usize, u8)`, `ConCtrl(u8)`, `ConRxWm(usize)`,
  `ConIrqHold(u32)`, `ConFirq(bool)`, `IrqMask(bool)`,
  `FirqMask(bool)`, `BlkIrq(bool)`, `BlkFirq(bool)`,
  `BlkIrqHold(u32)`。
- `enum Trigger` — `OnPc(addr, Action)` または `OnStep(n, Action)`。
- `BootSequencer` — `Vec<Trigger>` と「次の step カウンタ」を保持。
  `BootSequencer::new(triggers)` で生成。CPU ループは
  `seq.on_pre_step(&mut bus, regs_base, pc, &mut cpu)` と
  `seq.on_post_step(&mut bus, regs_base)` を呼んで、マッチする
  `OnPc` / `OnStep` トリガを順次発火。各トリガの発火は最大 1 回。
  診断 getter `console_missing_count()` /
  `block_missing_count()` / `mmu_missing_count()` は、対象デバイスが
  バス上に存在せずアクションが silent no-op になった回数を返す。
- `parse_boot_script(&str) -> Result<Vec<Trigger>, String>` — パーサ。
  失敗時 `Err(line N: ...)`。
- `emit_boot_template(name: &str) -> String` — 名前付きシナリオ
  (`netbsd_mvme147` 等) のサンプルブートスクリプトを返す。

```rust
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;
use em6809_core::io::IoBus;
use em6809_core::bootscript::{BootSequencer, parse_boot_script};

let script = "
    at_pc $0100: mode sys
    at_pc $0100: map 0=0
    at_step 1000: con_ctrl 0x55
";
let triggers = parse_boot_script(script).expect("valid script");
let mut seq = BootSequencer::new(triggers);

let mut bus = IoBus::new(Memory::new());
let mut cpu = Cpu::new();
cpu.reset(&mut bus);
let regs_base = 0xFFE0;
loop {
    seq.on_pre_step(&mut bus, regs_base, cpu.r.pc, &mut cpu);
    cpu.step(&mut bus, false);
    seq.on_post_step(&mut bus, regs_base);
}
```

完全なスクリプト文法と既知の落とし穴 (config-vs-script 順序、対象
デバイス不在時の silent no-op、MMU base 検証) は em6809 の
[`docs/en/config_and_boot_script.md`](https://github.com/hha0x617/em6809/blob/main/docs/en/config_and_boot_script.md)
を参照してください。
