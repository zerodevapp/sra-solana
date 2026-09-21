import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { web3 } from "@anchor-lang/core";

const root = process.cwd();
const handlerProgram = keypairAddress("keys/sra_handler-keypair.json");
const cctpProgram = keypairAddress("keys/sra_cctp_adapter-keypair.json");
const acrossProgram = idlAddress("idl/sra_across_adapter.json");

function read(relativePath: string): string {
  return readFileSync(path.join(root, relativePath), "utf8");
}

function keypairAddress(relativePath: string): string {
  const secret = Uint8Array.from(JSON.parse(read(relativePath)) as number[]);
  return web3.Keypair.fromSecretKey(secret).publicKey.toBase58();
}

function idlAddress(relativePath: string): string {
  return (JSON.parse(read(relativePath)) as { address: string }).address;
}

function anchorPrograms(section: string): Record<string, string> {
  const match = read("Anchor.toml").match(
    new RegExp(`\\[programs\\.${section}\\]\\n([\\s\\S]*?)(?=\\n\\[)`)
  );
  assert(match, `missing programs.${section} section`);
  return Object.fromEntries(
    [...match[1].matchAll(/^(\w+) = "([^"]+)"$/gm)].map((entry) => [
      entry[1],
      entry[2],
    ])
  );
}

function deployedPrograms(relativePath: string): Record<string, string> {
  const entries = [
    ...read(relativePath).matchAll(
      /--program-id ([1-9A-HJ-NP-Za-km-z]+).*target\/deploy\/(\w+)\.so/g
    ),
  ];
  return Object.fromEntries(entries.map((entry) => [entry[2], entry[1]]));
}

function authorityTargets(relativePath: string): Set<string> {
  const match = read(relativePath).match(/for program_id in ([^;]+); do/);
  assert(match, `missing program authority loop in ${relativePath}`);
  return new Set(match[1].trim().split(/\s+/));
}

test("deployment identities match program declarations and IDLs", () => {
  assert.equal(idlAddress("idl/sra_handler.json"), handlerProgram);
  assert.equal(idlAddress("idl/sra_cctp_adapter.json"), cctpProgram);
  assert.match(
    read("programs/sra-handler/src/lib.rs"),
    new RegExp(`declare_id!\\("${handlerProgram}"\\)`)
  );
  assert.match(
    read("programs/sra-cctp-adapter/src/lib.rs"),
    new RegExp(`declare_id!\\("${cctpProgram}"\\)`)
  );
  assert.match(
    read("crates/sra-common/src/lib.rs"),
    new RegExp(`declare_id!\\("${handlerProgram}"\\)`)
  );
});

test("Anchor networks use the deployed program identities", () => {
  const expected = {
    sra_across_adapter: acrossProgram,
    sra_cctp_adapter: cctpProgram,
    sra_handler: handlerProgram,
  };
  assert.deepEqual(anchorPrograms("localnet"), expected);
  assert.deepEqual(anchorPrograms("mainnet"), expected);
});

for (const workflow of [
  ".github/workflows/source-e2e-surfpool.yml",
  ".github/workflows/dest-e2e-surfpool.yml",
]) {
  test(`${workflow} installs built programs at their deployed identities`, () => {
    const expected = {
      sra_across_adapter: acrossProgram,
      sra_cctp_adapter: cctpProgram,
      sra_handler: handlerProgram,
    };
    assert.deepEqual(deployedPrograms(workflow), expected);
    assert.deepEqual(
      authorityTargets(workflow),
      new Set(Object.values(expected))
    );
  });
}
