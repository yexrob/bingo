import { createHash } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { deflateSync } from 'node:zlib'

const SIZE = 256
const GRID = 5
const PADDING = 28
const CELL = 40
const PALETTES = [
  ['#E9F2FF', '#2463A7'],
  ['#FDEDEE', '#B73D4C'],
  ['#EBF7EF', '#287A4B'],
  ['#FFF3DE', '#A55B16'],
  ['#F1ECFA', '#7252A3'],
  ['#E8F6F7', '#22747B'],
  ['#F7EDEF', '#A34669'],
  ['#EEF0F4', '#52617A'],
  ['#F2F6E8', '#637D28'],
  ['#FFF0E8', '#B45129'],
  ['#E9F3F1', '#356E64'],
  ['#F4EDF7', '#80508D'],
]

const outputDirectories = process.argv.slice(2).map((path) => resolve(path))
if (outputDirectories.length === 0) outputDirectories.push(resolve('assets/avatars'))

for (const directory of outputDirectories) await mkdir(directory, { recursive: true })

for (let index = 0; index < PALETTES.length; index += 1) {
  const id = `identicon-${String(index + 1).padStart(2, '0')}`
  const data = renderIdenticon(id, PALETTES[index])
  await Promise.all(
    outputDirectories.map((directory) => writeFile(resolve(directory, `${id}.png`), data)),
  )
}

console.log(`Generated ${PALETTES.length} identicons in ${outputDirectories.join(', ')}`)

function renderIdenticon(id, [background, foreground]) {
  const pixels = Buffer.alloc(SIZE * SIZE * 4)
  fill(pixels, hex(background))
  const seed = createHash('sha256').update(`bingo:${id}:v1`).digest()
  const cells = []
  for (let row = 0; row < GRID; row += 1) {
    const half = []
    for (let column = 0; column < 3; column += 1) {
      half.push(seed[row * 3 + column] < 146)
    }
    cells.push([half[0], half[1], half[2], half[1], half[0]])
  }
  if (cells.flat().filter(Boolean).length < 8) cells[2] = [true, true, true, true, true]
  const color = hex(foreground)
  for (let row = 0; row < GRID; row += 1) {
    for (let column = 0; column < GRID; column += 1) {
      if (cells[row][column]) {
        rect(pixels, PADDING + column * CELL, PADDING + row * CELL, CELL, CELL, color)
      }
    }
  }
  return png(pixels)
}

function hex(value) {
  return [
    Number.parseInt(value.slice(1, 3), 16),
    Number.parseInt(value.slice(3, 5), 16),
    Number.parseInt(value.slice(5, 7), 16),
    255,
  ]
}

function fill(pixels, color) {
  for (let offset = 0; offset < pixels.length; offset += 4) pixels.set(color, offset)
}

function rect(pixels, x, y, width, height, color) {
  for (let row = y; row < y + height; row += 1) {
    for (let column = x; column < x + width; column += 1) {
      pixels.set(color, (row * SIZE + column) * 4)
    }
  }
}

function png(pixels) {
  const scanlines = Buffer.alloc((SIZE * 4 + 1) * SIZE)
  for (let row = 0; row < SIZE; row += 1) {
    const target = row * (SIZE * 4 + 1)
    scanlines[target] = 0
    pixels.copy(scanlines, target + 1, row * SIZE * 4, (row + 1) * SIZE * 4)
  }
  const header = Buffer.alloc(13)
  header.writeUInt32BE(SIZE, 0)
  header.writeUInt32BE(SIZE, 4)
  header.set([8, 6, 0, 0, 0], 8)
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(scanlines, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

function chunk(type, data) {
  const name = Buffer.from(type)
  const output = Buffer.alloc(data.length + 12)
  output.writeUInt32BE(data.length, 0)
  name.copy(output, 4)
  data.copy(output, 8)
  output.writeUInt32BE(crc32(Buffer.concat([name, data])), data.length + 8)
  return output
}

function crc32(data) {
  let crc = 0xffffffff
  for (const byte of data) {
    crc ^= byte
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0)
    }
  }
  return (crc ^ 0xffffffff) >>> 0
}
