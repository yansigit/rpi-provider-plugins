// Shared sha256 pins for the ag-1 reference scripts.
//
// The upstream checked-in sources are hashed directly, but the packages also
// execute build output (`packages/core/dist`, installed `@cortexkit/*` and
// `@earendil-works/pi-ai` dist directories) that is gitignored in the pinned
// reference repo. Pin those exact bytes so the committed fixtures cannot be
// regenerated against a different build of the same source pin.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import { join, relative, sep } from 'node:path';

export async function sha256File(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

/// Hash every `.js` file under a build directory, sorted by posix relative path.
export async function sha256Tree(directory) {
  const files = [];
  const walk = async (dir) => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(path);
      } else if (entry.isFile() && entry.name.endsWith('.js')) {
        files.push(path);
      }
    }
  };
  await walk(directory);
  files.sort();
  const manifest = [];
  for (const file of files) {
    const name = relative(directory, file).split(sep).join('/');
    manifest.push(`${name} ${await sha256File(file)}`);
  }
  return {
    files: files.length,
    sha256: createHash('sha256').update(`${manifest.join('\n')}\n`).digest('hex'),
  };
}

/// Assert the exact bytes of one imported build file.
export async function pinnedFile(directory, file, expected) {
  const actual = await sha256File(join(directory, file));
  assert.equal(actual, expected, `sha256 changed for ${join(directory, file)}`);
  return actual;
}

/// Assert the aggregate bytes of an imported build directory (all `.js` files).
export async function pinnedTree(directory, expected) {
  const actual = await sha256Tree(directory);
  assert.equal(actual.files, expected.files, `file count changed for ${directory}`);
  assert.equal(actual.sha256, expected.sha256, `sha256 changed for ${directory}`);
  return actual;
}
