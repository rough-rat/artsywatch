#!/usr/bin/env python3
"""Reset the board, or reset it into the WCH USB bootloader, over its CDC serial port.

Uses the ESP32-style DTR/RTS sequences that ch58x-rs-kit's usb_log understands
(RemoteReset::EspStyle): RTS acts as EN (reset), DTR as IO0 (boot pin).

    tools/cdc_reset.py               # reset (restart the firmware)
    tools/cdc_reset.py --bootloader  # reset into the ROM bootloader (for wchisp)

No dependencies: uses termios ioctls directly (pyserial not needed).
"""

import argparse
import fcntl
import glob
import os
import struct
import sys
import termios
import time

DEFAULT_GLOB = "/dev/serial/by-id/usb-ch58x-rs-kit_*"


def set_lines(fd, dtr=None, rts=None):
    for line, value in ((termios.TIOCM_DTR, dtr), (termios.TIOCM_RTS, rts)):
        if value is None:
            continue
        req = termios.TIOCMBIS if value else termios.TIOCMBIC
        fcntl.ioctl(fd, req, struct.pack("I", line))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("port", nargs="?", help=f"serial port (default: {DEFAULT_GLOB})")
    parser.add_argument("--bootloader", action="store_true", help="enter the USB bootloader")
    args = parser.parse_args()

    port = args.port
    if port is None:
        ports = sorted(glob.glob(DEFAULT_GLOB))
        if len(ports) != 1:
            sys.exit(f"expected one port matching {DEFAULT_GLOB}, found {ports or 'none'}")
        port = ports[0]

    # O_NONBLOCK: don't wait for carrier; opening raises DTR and RTS (both set = no effect)
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        attrs = termios.tcgetattr(fd)
        attrs[2] |= termios.CLOCAL
        attrs[2] &= ~termios.HUPCL  # closing must not toggle the lines again
        termios.tcsetattr(fd, termios.TCSANOW, attrs)

        set_lines(fd, dtr=False, rts=True)  # hold reset
        time.sleep(0.1)
        if args.bootloader:
            set_lines(fd, dtr=True)  # boot pin low...
            set_lines(fd, rts=False)  # ...then release reset (esptool's classic sequence)
            time.sleep(0.05)
        else:
            set_lines(fd, rts=False)  # release reset, boot pin high
    finally:
        try:
            os.close(fd)
        except OSError:
            pass  # the device may already be gone
    print(f"{port}: {'reset into bootloader' if args.bootloader else 'reset'}")


if __name__ == "__main__":
    main()
