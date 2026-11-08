#!/usr/bin/env python3
"""Generate a 1024x1024 app icon PNG (stdlib only, no PIL).
Dark rounded tile + AIPG-green lightning bolt + three network nodes."""
import struct, zlib, math, sys

N = 1024
BG = (15, 17, 21)       # #0f1115 dark tile
GREEN = (74, 222, 128)  # #4ade80
BLUE = (56, 189, 248)   # #38bdf8

def rounded_alpha(x, y, n, r):
    # alpha for a rounded square covering the canvas with corner radius r
    cx = min(x, n - 1 - x)
    cy = min(y, n - 1 - y)
    if cx >= r or cy >= r:
        return 255
    dx = r - cx
    dy = r - cy
    d = math.hypot(dx, dy)
    if d <= r:
        return 255
    if d >= r + 1.5:
        return 0
    return int(255 * (1 - (d - r) / 1.5))

def point_in_poly(px, py, poly):
    inside = False
    j = len(poly) - 1
    for i in range(len(poly)):
        xi, yi = poly[i]; xj, yj = poly[j]
        if ((yi > py) != (yj > py)) and (px < (xj - xi) * (py - yi) / (yj - yi) + xi):
            inside = not inside
        j = i
    return inside

# lightning bolt polygon in a 0..1 space, then scaled
bolt = [(0.58,0.12),(0.34,0.56),(0.48,0.56),(0.40,0.88),(0.70,0.42),(0.54,0.42),(0.64,0.12)]
bolt = [(x*N, y*N) for x,y in bolt]

nodes = [(0.80,0.24,36),(0.82,0.74,30),(0.22,0.80,26)]  # x,y,radius (fraction)
nodes = [(x*N, y*N, r) for x,y,r in nodes]

def blend(base, over, a):
    return tuple(int(base[i]*(1-a)+over[i]*a) for i in range(3))

raw = bytearray()
for y in range(N):
    raw.append(0)  # png filter type 0 per row
    for x in range(N):
        r,g,b = BG
        # nodes (behind bolt), blue
        for nx,ny,nr in nodes:
            d = math.hypot(x-nx, y-ny)
            if d <= nr+1:
                a = 1.0 if d <= nr-1 else max(0.0, (nr+1-d)/2)
                r,g,b = blend((r,g,b), BLUE, a)
        # bolt, green with 2px soft edge via supersample of edge
        if point_in_poly(x+0.5, y+0.5, bolt):
            r,g,b = GREEN
        alpha = rounded_alpha(x, y, N, int(N*0.18))
        raw += bytes((r,g,b,alpha))

def chunk(tag, data):
    c = struct.pack(">I", len(data)) + tag + data
    return c + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)

png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
png += chunk(b"IEND", b"")
open(sys.argv[1] if len(sys.argv)>1 else "app-icon.png","wb").write(png)
print("wrote", sys.argv[1] if len(sys.argv)>1 else "app-icon.png", len(png), "bytes")
