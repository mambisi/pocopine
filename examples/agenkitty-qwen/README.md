# Agenkitty Qwen Example

This example runs `agenkitty` against Qwen through the OpenAI-compatible
provider.

The committed `.env` file contains placeholders only. For a real run, copy it
to `.env.local`, fill in the values from Alibaba Model Studio, and keep
`.env.local` uncommitted.

```sh
cp examples/agenkitty-qwen/.env examples/agenkitty-qwen/.env.local
```

Run from the workspace root:

```sh
cargo run -p agenkitty -- run \
  --provider qwen \
  --env-file examples/agenkitty-qwen/.env.local \
  --path examples/agenkitty-qwen \
  --format jsonl \
  "Reply with exactly: agenkitty qwen ok"
```

`QWEN_MODEL` may be either a bare model name such as `qwen-plus` or a full
model ref such as `qwen/qwen-plus`.
