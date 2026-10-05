import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const script = fileURLToPath(new URL('./build.mjs', import.meta.url));
function fixture(t) {
  const root = mkdtempSync(path.join(realpathSync(tmpdir()), 'gently-docs-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, 'docs');
  const out = path.join(root, 'site');
  mkdirSync(source);
  const put = (name, text) => {
    mkdirSync(path.dirname(path.join(source, name)), { recursive: true });
    writeFileSync(path.join(source, name), text);
  };
  put('README.md', '# Introduction\n');
  put('SUMMARY.md', '# Summary\n\n* [Introduction](README.md)\n');
  return { root, source, out, put, build: () => spawnSync(process.execPath, [script, '--source', source, '--out', out], { encoding: 'utf8' }) };
}
function succeeds(result) { assert.equal(result.status, 0, result.stderr || result.stdout); }
function fails(result, pattern) { assert.notEqual(result.status, 0); assert.match(result.stderr, pattern); }
function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap(entry => entry.isDirectory()
    ? files(path.join(dir, entry.name)).map(name => `${entry.name}/${name}`) : [entry.name]).sort();
}

test('renders navigation, nested README routes, fragments and literal fences', t => {
  const f = fixture(t);
  f.put('README.md', '---\ndescription: private front matter\n---\n# Introduction\n\n[Guide](guide/README.md?mode=1#some-heading)\n\n```md\n[x](guide/README.md)\n<script>alert(1)</script>\n```\n');
  f.put('guide/README.md', '# Guide\n\n## Some heading\n\n[Home](../README.md)\n');
  f.put('SUMMARY.md', '# Summary\n\n* [Introduction](README.md)\n\n## Guides\n\n* [Guide](guide/README.md)\n');
  succeeds(f.build());
  assert.deepEqual(files(f.out), ['guide/index.html', 'index.html', 'site.css']);
  const home = readFileSync(path.join(f.out, 'index.html'), 'utf8');
  const guide = readFileSync(path.join(f.out, 'guide/index.html'), 'utf8');
  assert.match(home, /href="guide\/index\.html\?mode=1#some-heading"/);
  assert.match(home, /\[x\]\(guide\/README\.md\)/);
  assert.match(home, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.doesNotMatch(home, /private front matter/);
  assert.match(guide, /href="\.\.\/index\.html"/);
  assert.match(guide, /href="\.\.\/site\.css"/);
  assert.match(guide, /id="some-heading"/);
  assert.match(home, /<h2>Guides<\/h2>/);
});

test('encodes generated routes and gives repeated headings distinct anchors', t => {
  const f = fixture(t);
  f.put('README.md', '# Introduction\n\n[Reserved](guide/a%23b%25.md#repeat)\n');
  f.put('guide/a#b%.md', '# Reserved\n\n## Repeat\n\n## Repeat\n');
  succeeds(f.build());
  assert.match(readFileSync(path.join(f.out, 'index.html'), 'utf8'), /href="guide\/a%23b%25\.html#repeat"/);
  const page = readFileSync(path.join(f.out, 'guide/a#b%.html'), 'utf8');
  assert.match(page, /id="repeat"/);
  assert.match(page, /id="repeat-1"/);
});

test('escapes page titles and navigation and disables active HTML and unsafe URLs', t => {
  const f = fixture(t);
  f.put('README.md', '# <script>alert("title")</script>\n\n<script>alert("body")</script>\n\n[x](javascript:alert(1)) [x](java&#x73;cript:alert(1)) [x](vbscript:msgbox(1)) [x](data:text/html,test) [x](file:///etc/passwd)\n\n![x](data:image/png;base64,AA==)\n\n[Safe](https://example.com/?q=%22)\n');
  f.put('SUMMARY.md', '# Summary\n\n## <img src=x onerror=alert(1)>\n\n* [<script>alert("nav")</script>](README.md)\n');
  succeeds(f.build());
  const page = readFileSync(path.join(f.out, 'index.html'), 'utf8');
  assert.doesNotMatch(page, /<script|<img|href="(?:javascript:|data:|file:)/i);
  assert.match(page, /<title>&lt;script&gt;alert\(&quot;title&quot;\)&lt;\/script&gt;/);
  assert.match(page, /&lt;script&gt;alert\(&quot;nav&quot;\)&lt;\/script&gt;/);
  assert.match(page, /href="https:\/\/example\.com\//);
  assert.match(page, /Content-Security-Policy/);
});

test('publishes only Markdown pages and generated CSS', t => {
  const f = fixture(t);
  f.put('settings.json', '{"synthetic":"DO_NOT_PUBLISH"}');
  f.put('settings.yaml', 'synthetic: DO_NOT_PUBLISH');
  f.put('package.json', '{}');
  f.put('unlisted.md', '# Unlisted but public\n');
  f.put('_book/stale.md', '# DO_NOT_PUBLISH\n');
  f.put('node_modules/package/leak.md', '# DO_NOT_PUBLISH\n');
  succeeds(f.build());
  assert.deepEqual(files(f.out), ['index.html', 'site.css', 'unlisted.html']);
});

test('rejects traversal and broken local links without publishing partial output', t => {
  for (const link of ['../outside.md', '%2e%2e/outside.md', '/outside.md', 'missing.md', 'settings.json']) {
    const f = fixture(t);
    f.put('README.md', `# Introduction\n\n[unsafe](${link})\n`);
    fails(f.build(), /local link|outside|Markdown/i);
    assert.equal(readdirSync(f.root).includes('site'), false);
  }
});

test('rejects invalid summary targets and colliding page routes', t => {
  const f = fixture(t);
  f.put('SUMMARY.md', '# Summary\n\n* [Missing](missing.md)\n');
  fails(f.build(), /local link|target/i);
  f.put('SUMMARY.md', '# Summary\n\n* [Introduction](README.md)\n');
  f.put('index.md', '# Collision\n');
  fails(f.build(), /collid/i);
});

test('does not follow source symlink files or directories', t => {
  for (const directory of [false, true]) {
    const f = fixture(t);
    const target = path.join(f.root, 'outside');
    if (directory) { mkdirSync(target); writeFileSync(path.join(target, 'leak.md'), '# NEVER_READ\n'); }
    else writeFileSync(target, '# NEVER_READ\n');
    symlinkSync(target, path.join(f.source, directory ? 'linked' : 'linked.md'));
    fails(f.build(), /symlink/i);
    assert.equal(readdirSync(f.root).includes('site'), false);
  }
});

test('rejects output symlinks, symlink ancestors, and unmanaged existing output', t => {
  for (const kind of ['root', 'ancestor', 'file', 'foreign']) {
    const f = fixture(t);
    const outside = path.join(f.root, 'outside');
    mkdirSync(outside);
    writeFileSync(path.join(outside, 'marker'), 'preserve');
    if (kind === 'root') symlinkSync(outside, f.out);
    else if (kind === 'ancestor') { symlinkSync(outside, path.join(f.root, 'linked')); f.out = path.join(f.root, 'linked/site'); }
    else {
      mkdirSync(f.out);
      if (kind === 'file') symlinkSync(path.join(outside, 'marker'), path.join(f.out, 'index.html'));
      else writeFileSync(path.join(f.out, 'foreign.html'), 'preserve');
    }
    const result = spawnSync(process.execPath, [script, '--source', f.source, '--out', f.out], { encoding: 'utf8' });
    fails(result, /symlink|unmanaged/i);
    assert.equal(readFileSync(path.join(outside, 'marker'), 'utf8'), 'preserve');
    if (kind === 'foreign') assert.equal(readFileSync(path.join(f.out, 'foreign.html'), 'utf8'), 'preserve');
  }
});

test('rejects output inside or above source and refreshes only previously generated output', t => {
  const f = fixture(t);
  for (const out of [f.root, f.source, path.join(f.source, '_site')]) {
    fails(spawnSync(process.execPath, [script, '--source', f.source, '--out', out], { encoding: 'utf8' }), /overlap/i);
  }
  f.put('old.md', '# Old\n');
  succeeds(f.build());
  rmSync(path.join(f.source, 'old.md'));
  f.put('new.md', '# New\n');
  succeeds(f.build());
  assert.deepEqual(files(f.out), ['index.html', 'new.html', 'site.css']);
});
