import assert from "node:assert/strict";
import test from "node:test";
import { web3 } from "@anchor-lang/core";
import {
  AccountRole,
  getBase64EncodedWireTransaction,
  getBase64Encoder,
  type Blockhash,
} from "@solana/kit";
import {
  compileV1Transaction,
  toKitInstruction,
  toSafeBlockHeight,
} from "../scripts/initialize-fee-config";

test("converts account roles and data", () => {
  const instruction = new web3.TransactionInstruction({
    programId: web3.Keypair.generate().publicKey,
    keys: [
      {
        pubkey: web3.Keypair.generate().publicKey,
        isSigner: true,
        isWritable: true,
      },
      {
        pubkey: web3.Keypair.generate().publicKey,
        isSigner: true,
        isWritable: false,
      },
      {
        pubkey: web3.Keypair.generate().publicKey,
        isSigner: false,
        isWritable: true,
      },
      {
        pubkey: web3.Keypair.generate().publicKey,
        isSigner: false,
        isWritable: false,
      },
    ],
    data: Buffer.from([1, 2, 3]),
  });
  const converted = toKitInstruction(instruction);

  assert.equal(converted.accounts?.[0].role, AccountRole.WRITABLE_SIGNER);
  assert.equal(converted.accounts?.[1].role, AccountRole.READONLY_SIGNER);
  assert.equal(converted.accounts?.[2].role, AccountRole.WRITABLE);
  assert.equal(converted.accounts?.[3].role, AccountRole.READONLY);
  assert.deepEqual(converted.data, new Uint8Array([1, 2, 3]));
});

test("narrows valid block heights", () => {
  assert.equal(toSafeBlockHeight(0n), 0);
  assert.equal(
    toSafeBlockHeight(BigInt(Number.MAX_SAFE_INTEGER)),
    Number.MAX_SAFE_INTEGER
  );
});

test("rejects block heights that cannot be safely narrowed", () => {
  const expected = /must be between 0 and Number\.MAX_SAFE_INTEGER/;

  assert.throws(() => toSafeBlockHeight(-1n), expected);
  assert.throws(
    () => toSafeBlockHeight(BigInt(Number.MAX_SAFE_INTEGER) + 1n),
    expected
  );
});

test("builds a 0x81-prefixed transaction", async () => {
  const payer = web3.Keypair.generate();
  const instruction = web3.SystemProgram.transfer({
    fromPubkey: payer.publicKey,
    toPubkey: web3.Keypair.generate().publicKey,
    lamports: 1,
  });
  const transaction = await compileV1Transaction(instruction, payer.secretKey, {
    blockhash: "11111111111111111111111111111111" as Blockhash,
    lastValidBlockHeight: 1n,
  });
  const wire = getBase64Encoder().encode(
    getBase64EncodedWireTransaction(transaction)
  );

  assert.equal(wire[0], 0x81);
});
