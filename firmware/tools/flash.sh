#!/usr/bin/env bash
# cargo runner: flash an ELF over USB if possible, else with the WCH-LinkE probe.
#
# 1. Firmware running with a CDC port: reset it into the bootloader (tools/cdc_reset.py),
#    then flash with wchisp.
# 2. Bootloader already present (4348:55e0, e.g. BOOT held while plugging in): wchisp.
# 3. Otherwise: wlink (probe).
#
# Force a method with FLASH_WITH=usb|wlink.
set -euo pipefail

elf="$1"
here="$(cd "$(dirname "$0")" && pwd)"
bootloader_present() { lsusb -d 4348:55e0 >/dev/null 2>&1; }
cdc_port() { compgen -G "/dev/serial/by-id/usb-ch58x-rs-kit_*" | head -n1; }

flash_usb() {
    if ! bootloader_present; then
        port="$(cdc_port || true)"
        if [ -z "$port" ]; then
            echo "flash.sh: no CDC port and no bootloader on USB" >&2
            return 1
        fi
        "$here/cdc_reset.py" --bootloader "$port"
        for _ in $(seq 50); do bootloader_present && break; sleep 0.1; done
        bootloader_present || { echo "flash.sh: bootloader did not appear" >&2; return 1; }
        sleep 0.3
    fi
    wchisp flash "$elf"
}

case "${FLASH_WITH:-auto}" in
    usb) flash_usb ;;
    wlink) wlink -v flash "$elf" ;;
    auto)
        if bootloader_present || [ -n "$(cdc_port || true)" ]; then
            flash_usb
        else
            wlink -v flash "$elf"
        fi
        ;;
    *) echo "flash.sh: FLASH_WITH must be usb, wlink or auto" >&2; exit 2 ;;
esac
