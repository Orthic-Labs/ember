#!/usr/bin/env python3
"""Publish approved raster artwork to app consumers. Pillow only; no app build."""
import base64
import hashlib
import io
import json
import re
import struct
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'assets/app-icon'
PACK = ROOT / 'assets/brand/aperture-flame-v1'
APP_ID = 'com.orthiclabs.ember'

def master():
    source = PACK / 'source/master-rgba.png'
    expected = json.loads((PACK / 'manifest.json').read_text())['master_sha256']
    if hashlib.sha256(source.read_bytes()).hexdigest() != expected:
        raise ValueError('Approved icon master changed')
    with Image.open(source) as image:
        if image.mode != 'RGBA' or image.size[0] != image.size[1]:
            raise ValueError('Approved master must be square RGBA')
        return image.copy()

def render(size, macos=False):
    extent = round(size * 512 / 636) if macos else size
    mark = master().resize((extent, extent), Image.Resampling.LANCZOS)
    if not macos:
        return mark
    output = Image.new('RGBA', (size, size))
    offset = (size - extent) // 2
    output.paste(mark, (offset, offset))
    return output

def embedded_svg(png):
    encoded = base64.b64encode(png).decode('ascii')
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" '
            'role="img" aria-labelledby="title"><title id="title">Ember aperture &amp; flame</title>'
            '<image width="1024" height="1024" href="data:image/png;base64,' + encoded + '"/></svg>\n').encode()

def main():
    render(1024).save(ASSETS / 'ember-1024.png')
    render(512, macos=True).save(ASSETS / 'ember-macos-512.png')
    (ASSETS / 'ember-reverse-1024.png').write_bytes((PACK / 'png/reverse/ember-1024.png').read_bytes())
    svg = embedded_svg((ASSETS / 'ember-1024.png').read_bytes())
    (ASSETS / 'ember.svg').write_bytes(svg)
    for size in (16, 24, 32, 48, 64, 128, 256, 512):
        target = ASSETS / f'hicolor/{size}x{size}/apps/{APP_ID}.png'
        target.parent.mkdir(parents=True, exist_ok=True)
        render(size).save(target)
    target = ASSETS / f'hicolor/scalable/apps/{APP_ID}.svg'
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(svg)
    sizes = (16, 20, 24, 32, 40, 48, 64, 128, 256)
    entries = []
    offset = 6 + 16 * len(sizes)
    payloads = []
    for size in sizes:
        png = io.BytesIO()
        render(size).save(png, format='PNG')
        payload = png.getvalue()
        dim = size if size < 256 else 0
        entries.append(struct.pack('<BBBBHHII', dim, dim, 0, 0, 1, 32, len(payload), offset))
        payloads.append(payload)
        offset += len(payload)
    (ASSETS / 'ember.ico').write_bytes(struct.pack('<HHH', 0, 1, len(sizes)) + b''.join(entries + payloads))
    render(1024, macos=True).save(ASSETS / 'ember.icns', append_images=[render(size, macos=True) for size in (128, 256, 512)])
    favicon = io.BytesIO()
    render(32).save(favicon, format='PNG')
    page = ROOT / 'apps/lightcraft-web/index.html'
    text = page.read_text(encoding='utf-8')
    text, count = re.subn(r'<link rel="icon"[^>]*>', '<link rel="icon" type="image/png" href="data:image/png;base64,'
                         + base64.b64encode(favicon.getvalue()).decode('ascii') + '">', text, count=1)
    if count != 1:
        raise ValueError('WASM favicon declaration missing')
    page.write_text(text, encoding='utf-8', newline='\n')
    print('Approved Ember aperture/flame published to native, installer & web assets.')

if __name__ == '__main__':
    main()
