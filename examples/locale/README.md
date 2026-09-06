# Locale example

From the repository root:

```sh
pocopine run --path examples/locale --port 3088
```

Open http://127.0.0.1:3088. Change the name, increase the item count, switch
languages, and request a greeting or public error from the server. The server
receives the committed browser language on every request.

The CLI reads `pocopine.toml`, extracts active browser and host references, and
generates `t` before compiling. `include_translations!()` includes that output;
there is no application build script. The example has an isolated Cargo
workspace because a direct workspace Cargo invocation does not run the CLI's
generation stage.

`client::main` waits for the parallel catalog download before mounting.
`server::recipient_message` demonstrates explicit recipient-language rendering
for a worker; the translated result is created when the job runs.
