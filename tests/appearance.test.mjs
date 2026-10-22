import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import ts from "typescript";
const module = { exports: {} };
const code = ts.transpileModule(readFileSync(new URL("../src/theme.ts", import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
vm.runInNewContext(`(function(module,exports){${code}\n})`)(module, module.exports);
const { ThemeController } = module.exports;
function fixture(saved, dark = false) {
  let change; const applied = [], stored = [];
  const media = { matches: dark, addEventListener: (_, listener) => { change = listener; } };
  const controller = new ThemeController({ getItem: () => saved, setItem: (...args) => stored.push(args) }, media, value => applied.push(value));
  return { controller, media, applied, stored, change: () => change() };
}
test("system theme follows live OS changes; explicit choices stay fixed and persist", () => {
  const { controller: c, media, applied, stored, change } = fixture(null);
  assert.equal(c.resolved, "light"); media.matches = true; change(); assert.equal(applied.at(-1), "dark");
  c.setPreference("light"); assert.equal(c.resolved, "light"); change(); assert.equal(applied.at(-1), "light");
  assert.deepEqual(stored.at(-1), ["sivro-theme", "light"]);
  c.setPreference("system"); assert.equal(c.resolved, "dark");
  c.setPreference("invalid"); assert.equal(c.preference, "system");
});
test("saved preferences restore, invalid storage is ignored and storage failures are harmless", () => {
  assert.equal(fixture("dark").controller.resolved, "dark");
  assert.equal(fixture("light", true).controller.resolved, "light");
  assert.equal(fixture("oops", true).controller.resolved, "dark");
  const c = new ThemeController({ getItem() { throw new Error(); }, setItem() { throw new Error(); } }, null, () => {});
  c.setPreference("dark"); assert.equal(c.resolved, "dark");
});
const luminance = hex => {
  const rgb = [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16) / 255).map(c => c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4);
  return rgb[0] * .2126 + rgb[1] * .7152 + rgb[2] * .0722;
};
const contrast = (a, b) => { const [lo, hi] = [luminance(a), luminance(b)].sort((a, b) => a - b); return (hi + .05) / (lo + .05); };
test("both palettes provide AA contrast for body, secondary, muted, selected and primary action text", () => {
  const css = readFileSync(new URL("../src/themes.css", import.meta.url), "utf8");
  for (const [label, block] of [["light", css.split(':root[data-theme="dark"]')[0]], ["dark", css.split(':root[data-theme="dark"]')[1].split("}")[0]]]) {
    const tokens = Object.fromEntries([...block.matchAll(/(--[\w-]+):\s*(#[\da-f]{6})/g)].map(([, name, color]) => [name, color]));
    for (const text of ["--text-primary", "--text-secondary", "--text-muted", "--text-faint"]) assert.ok(contrast(tokens[text], tokens["--surface"]) >= 4.5, `${label}: ${text} contrast`);
    assert.ok(contrast(tokens["--on-accent"], tokens["--accent"]) >= 4.5, `${label}: button contrast`);
    assert.ok(contrast(tokens["--text-primary"], tokens["--selection"]) >= 4.5, `${label}: selected text contrast`);
    for (const [text, background] of [["--teal-700", "--teal-soft"], ["--sky-700", "--sky-soft"], ["--plum-700", "--plum-soft"], ["--mint-700", "--mint-soft"], ["--coral-600", "--coral-soft"], ["--honey-600", "--honey-soft"]]) assert.ok(contrast(tokens[text], tokens[background]) >= 4.5, `${label}: ${text} badge contrast`);
  }
});
