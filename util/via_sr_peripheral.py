import os
import re
import socket
import selectors
import time
from selectors import DefaultSelector


HOST = '127.0.0.1'
PORT = 10001
PATH = "~/.emma/sock/via6522"

TOKEN_RE = re.compile(r"(?P<port>[AaBb])(?P<pins>[0-9A-Fa-f]{2})|(?P<ctrl>[Cc][AaBb])(?P<pin>[1-2])(?P<state>[0-1])")


class Transport:

    def __init__(self, path: str):
        self.path = os.path.expanduser(path)
        self.sock = None
        self.selector = DefaultSelector()
        self.has_prior = False

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(self.path)
        self.sock.sendall("\n".encode("US-ASCII"))
        self.sock.setblocking(False)
        self.selector.register(self.sock, selectors.EVENT_READ)

    def reader(self):
        buf = bytes()
        index = 0
        while True:
            if index >= len(buf):
                self.selector.select()
                buf = self.sock.recv(1024)
                if not buf:
                    break
                index = 0
            c = chr(buf[index]).upper()
            yield c
            index += 1

    def send_clock(self, rising=False):
        level = 1 if rising else 0
        message = f"CB1{level}"
        print(message)
        message = (" " if self.has_prior else "") + message
        self.has_prior = True
        self.sock.sendall(message.encode("US-ASCII"))

    def send_bit(self, v, bit_num):
        mask = 1 << (7 - bit_num)
        bit_val = 1 if v & mask else 0
        message = f"CB2{bit_val}"
        print(message)
        self.sock.sendall(message.encode("US-ASCII"))


def scanner(reader):
    while c := next(reader):
        if c == "A" or c == "B":
            d1 = next(reader)
            d2 = next(reader)
            yield c, int((d1 + d2), 16)
        elif c == "C":
            c = next(reader)
            d1 = next(reader)
            d2 = next(reader)
            yield "C" + c, int(d1), int(d2)
        else:
            pass


def shift_in_int_clock(v, scanner, transport: Transport) -> int:
    i = 0
    v = 0
    while i < 8:
        transport.send_clock(False)
        while c := next(scanner):
            print(c)
            if c[0] == "CB" and c[1] == 2:
                mask = 1 << (7 - i)
                bit = c[2]
                v = v | mask if bit else v & ~mask
                break
        if not c:
            raise RuntimeError("transport disconnected")
        transport.send_clock(True)
        i += 1
    return v


def shift_out_int_clock(v, scanner, transport: Transport):
    i = 0
    while i < 8:
        time.sleep(0.001)
        transport.send_clock(False)
        transport.send_bit(v, i)
        time.sleep(0.001)
        transport.send_clock(True)
        i += 1


def shift_in_ext_clock(v, scanner, transport: Transport) -> int:
    i = 0
    v = 0
    latched = False
    while i < 8 and (c := next(scanner)):
        print(c)
        if c[0] == "CB":
            if c[1] == 1:
                if c[2] == 0:
                    # latch on the falling edge
                    latched = True
                else:
                    # next bit on rising edge
                    latched = False
                    i += 1
            elif latched:
                mask = 1 << (7 - i)
                bit = c[2]
                v = v | mask if bit else v & ~mask

    if i < 8:
        raise RuntimeError("shift not completed")
    return v


def shift_in_ext_clock_free_running(v, scanner, transport: Transport) -> int:
    while True:
        i = 0
        v = 0
        latched = False
        while i < 8 and (c := next(scanner)):
            print(i, c)
            if c[0] == "CB":
                if c[1] == 1:
                    if c[2] == 0:
                        # latch on the falling edge
                        latched = True
                    elif latched:
                        # next bit on rising edge
                        latched = False
                        i += 1
                elif latched:
                    mask = 1 << (7 - i)
                    bit = c[2]
                    v = v | mask if bit else v & ~mask

        if i < 8:
            raise RuntimeError("shift not completed")
        print(f"{v:02x}")


def shift_out_ext_clock(v, scanner, transport: Transport) -> int:
    i = 0
    while i < 8 and (c := next(scanner)):
        print(c)
        if c[0] == "CB" and c[1] == 1:
            if c[2] == 0:
                transport.send_bit(v, i)
            else:
                # next bit on rising edge
                i += 1
    if i < 8:
        raise RuntimeError("shift not completed")
    return v


def await_start(scanner) -> bool:
    triggered = False
    while message := next(scanner):
        print(message)
        if message[0] == "B":
            reset_bit = message[1] & 0b00100000
            if not reset_bit and not triggered:
                triggered = True
            elif reset_bit and triggered:
                return True
    return False


def shift_operation(v, scanner, transport: Transport, op) -> int:
    if not await_start(scanner):
        raise RuntimeException("shift operation never started")
    return op(v, scanner, transport)


def run():
    transport = Transport(PATH)
    transport.connect()
    print("connected")
    r = iter(transport.reader())
    s = iter(scanner(r))
    v = shift_operation(0, s, transport, shift_in_ext_clock_free_running)
    print(f"{v:02x}")


if __name__ == "__main__":
    run()
