// Node/V8 `JSON.parse` diagnostic corpus. Emits the exact message Node 24.22.0
// raises for every input so the Rust port can reproduce OAuth/project bodies
// byte for byte.
//
//   RPI_REFERENCE_OUTPUT=tests/fixtures/ag1-json-errors.json \
//   node scripts/ag1-json-errors-reference.mjs --write
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const output = resolve(process.env.RPI_REFERENCE_OUTPUT);
const write = process.argv.includes('--write');

const inputs = new Set([
  // Valid controls.
  'null', 'true', 'false', '0', '-0', '123', '-123', '1.5', '1e10', '1E-10',
  '""', '"abc"', '[]', '{}', '[1,2,3]', '{"a":1}', '{"a":[1,{"b":null}]}',
  '  {\n "a" : 1 }  ', '"\\u0041\\n\\t\\\\\\/\\""', '123456789012345678901234567890',
  '1e308', '[0.1,2e3,-4.5e-6]', '{"":"",}', '{"":""}', '[[]]', '[{}]', '{"a":{}}',
  '\t\r\n[]\t\r\n', '{"a":1,"a":2}', '{"\\ud800":1}', '\ufeff{}',
  // Tokens, literals and identifiers.
  'invalid', 'NaN', 'Infinity', 'undefined', '[object Object]', 'NaNx', 'Infinityx',
  'undefinedd', 'Na', 'Inf', 'undef', 'N', 'i', 'I', 'u', 'U', 'f', 't', 'n',
  'True', 'NULL', 'NaN ', 'NaN]', 'truefalse', 'nulll', 'nullx', 'truex', 'falsex',
  'tru', 'nul', 'fals', 'trux', 'falsx',
  // Numbers.
  '01', '1 2', '1e', '1e+', '1e-', '1E', '0.', '-0.', '0x1', '1_0', '1n',
  '0.0.0', '00', '0e', '-x', '-', '--1', '-Infinity', '+1', '+Infinity', '.5', '1.',
  '1..2', '1.e2', '-.5', '{"a":01}', '{"a":+1}', '{"a":-}', '[0x1]', '[1n]', '[+1]',
  '[.1]',
  // Strings.
  '"abc', '"\\q"', '"\\uZZZZ"', '"\\u12"', '"\\u"', '"\\"', '"\\\\', '"a\\',
  '"tab\tchar"', '"newline\nchar"', '"ctrl\u0001char"', '[\"\\u12\"]', '[\"\\u123G\"]',
  '[\"unterminated', '{"unterminated',
  // Structures.
  '{', '{"a":}', '{a:1}', '{"a" 1}', '{"a"', '{"a":', '{"a":1', '{"a":1,',
  '{"a":1,}', '{"a":1 "b":2}', '{"a":1"b":2}', '{"a":{"b":}}', '{"a":1}}',
  '{"k":1,', '{,}', '{:1}', '{1:2}', '[', ']', '}', '[,]', '[1,,2]', '[1,2', '[1,2,3,',
  '["x",]', '[]]', '{"a":1}{"b":2}', '[1 2]', '["a" "b"]', '[NaN]', '[undefined]',
  '[Infinity]', '{"a":NaN}', '{"a":undefined}', '{"a":Infinity}', '[1,2,3]\n\n[]',
  // Line/column and context.
  'x', '[x]', '[1,x]', '{\"a\":x}', '\n\nx', 'a\nb', '[\n1,\nx]', '{\n\"a\":\nx\n}',
  '[\r\n1,\r\nx]', '1234567890 bad', '{\"a\":1}\n\nx', '\u00e9', 'caf\u00e9',
  '\"caf\u00e9\"x', '[\"\u00e9\",x]', '\u00a0{}', '\u2003x',
  'x'.repeat(200), 'a'.repeat(30), 'aaaa' + 'b'.repeat(30),
  '{\"key\":\"value\",\"bad\":}',
  '[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,x]',
  '[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,x,1]',
  '{\"a\":true,\"b\":false,\"c\":null,\"d\":x}',
  '{\"aaaa\":\"bbbb\",\"cccc\":\"dddd\",\"eeee\":x}',
]);

// Deterministic truncations and mutations of representative valid documents.
for (const source of ['{"access_token":"a","refresh_token":"r","expires_in":1800}', '[1,2,3]', '"hello"', '{"a":{"b":[1,2]}}']) {
  for (let end = 1; end < source.length; end += 1) inputs.add(source.slice(0, end));
}
for (const source of ['{"a":1}', '[1,2]', 'null']) {
  inputs.add(source + 'x');
  inputs.add(source + ' ');
  inputs.add(' ' + source);
  inputs.add(source + '\n\n');
  inputs.add(source.replace('{', '}'));
}

const cases = [...inputs].map((input) => {
  let message = null;
  try {
    JSON.parse(input);
  } catch (error) {
    message = error.message;
  }
  return { input, message };
});
const document =
  JSON.stringify({ referenceRuntime: process.version, cases }, null, 2) + '\n';
if (write) {
  await writeFile(output, document);
  console.log(JSON.stringify({ cases: cases.length, malformed: cases.filter((c) => c.message).length }));
} else {
  assert.equal(await readFile(output, 'utf8'), document, 'fixture differs; run with --write');
  console.log(JSON.stringify({ checked: true, cases: cases.length }));
}
