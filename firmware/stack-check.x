/* Everything between the end of .bss and the top of RAM is stack. Measured peak use is ~1.4 KB
 * (stack painting, 2026-11). Keep a generous margin: an overflow silently corrupts .bss (the
 * BLE library's globals sit at its end), which looks like random boot failures.
 * Large stack frames to avoid: e.g. decoding the LED layout at run time took a 9.3 KB frame,
 * see build.rs. */
ASSERT(_stack_top - _ebss >= 0x1000,
       "artsywatch: less than 4 KB of RAM left for the stack (end of .bss is too close to the top of RAM)")
