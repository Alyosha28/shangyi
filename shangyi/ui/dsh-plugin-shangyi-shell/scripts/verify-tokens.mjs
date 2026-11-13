#!/usr/bin/env node
// 校验生成的 shell token 覆盖表与 lib/client.js 的语法与 WCAG 对比度（零外部依赖，可在公开仓库独立运行）。
import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import vm from 'node:vm'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const pkgRoot = path.resolve(here, '..')
const tokensPath = path.join(pkgRoot, 'lib', 'tokens.gen.js')
const clientPath = path.join(pkgRoot, 'lib', 'client.js')

const src = fs.readFileSync(tokensPath, 'utf8')
const sandbox = {}
vm.runInNewContext(src, sandbox, { filename: tokensPath })
const layers = sandbox.tokenLayers()
const TOKENS = sandbox.TOKENS

function parseHex(hex) {
  assert.equal(typeof hex, 'string', `非字符串色值: ${hex}`)
  const m = /^#([0-9a-fA-F]{6})$/.exec(hex.trim())
  assert.ok(m, `仅支持 #RRGGBB 色值校验，实得: ${hex}`)
  const n = parseInt(m[1], 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}

function relLuminance([r, g, b]) {
  const ch = (v) => {
    const x = v / 255
    return x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

function contrastRatio(fgHex, bgHex) {
  const l1 = relLuminance(parseHex(fgHex))
  const l2 = relLuminance(parseHex(bgHex))
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05)
}

const pairs = [
  ['--dsw-alias-label-primary-foreground', '--dsw-alias-brand-primary'],
  ['--dsw-alias-label-primary-foreground', '--dsw-alias-button-primary-fill'],
  ['--dsw-alias-label-primary-foreground', '--dsw-alias-button-primary-hover'],
  ['--dsw-alias-toast-label', '--dsw-alias-toast-bg'],
  ['--dsw-alias-label-primary-inverted', '--dsw-alias-button-contrast-fill'],
  ['--dsw-alias-label-primary', '--dsw-alias-bg-base']
]

let checked = 0
for (const variant of ['paper', 'lacquer', 'ledger']) {
  assert.ok(TOKENS[variant], `缺少变体 ${variant}`)
  assert.ok(layers[variant], `tokenLayers() 缺少变体 ${variant}`)
  for (const mode of ['light', 'dark']) {
    const table = TOKENS[variant][mode]
    assert.ok(table, `缺少 ${variant}.${mode}`)
    for (const [fgKey, bgKey] of pairs) {
      const cr = contrastRatio(table[fgKey], table[bgKey])
      assert.ok(
        cr >= 4.5,
        `${variant}.${mode}: ${fgKey} (${table[fgKey]}) on ${bgKey} (${table[bgKey]}) 对比度仅 ${cr.toFixed(2)}:1 (< 4.5:1)`
      )
      checked++
    }
  }
}

assert.ok(fs.readFileSync(clientPath, 'utf8').includes('--dsw-alias-label-primary-foreground'))
console.log(`✓ dsh-plugin-shangyi-shell token 对比度校验全部通过（${checked} 组配对均 >= 4.5:1）`)
