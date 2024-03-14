from pwn import *
from hashlib import sha256
import string
from pwnlib.util.iters import mbruteforce

r = remote('localhost',1337)

table = string.printable
def PoW():
    r.recvuntil(b'sha256(\'')
    suffix = r.recv(8).decode()
    print(suffix)
    proof = mbruteforce(lambda x: sha256((suffix+x).encode()).hexdigest().startswith('0'*6), table, length=4, method='fixed')
    if proof:
        r.sendafter(b'? = ', proof)
    
PoW()
r.interactive()