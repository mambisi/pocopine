import assert from 'node:assert/strict';
import { Upload } from 'tus-js-client';

const baseUrl = process.argv[2];
assert.ok(baseUrl, 'usage: node tus_js_client_e2e.mjs <base-url>');

const endpoint = new URL('/__pocopine/storage/tus/v1/avatars/uploads', baseUrl).href;
const payload = Buffer.from('hello from tus-js-client', 'utf8');
const cookie = 'pocopine_storage_anon=e2e-anon';
const requests = [];
const responses = [];

function uploadWithTus(options = {}) {
  return new Promise((resolve, reject) => {
    let upload;
    upload = new Upload(payload, {
      endpoint,
      chunkSize: 5,
      retryDelays: null,
      storeFingerprintForResuming: false,
      removeFingerprintOnSuccess: true,
      metadata: {
        filename: 'photo.txt',
        filetype: 'text/plain',
      },
      headers: {
        Cookie: cookie,
        'Sec-Fetch-Site': 'same-origin',
      },
      onBeforeRequest(req) {
        requests.push({
          method: req.getMethod(),
          url: req.getURL(),
          offset: req.getHeader('Upload-Offset') ?? null,
        });
      },
      onAfterResponse(req, res) {
        responses.push({
          method: req.getMethod(),
          status: res.getStatus(),
          offset: res.getHeader('Upload-Offset') ?? null,
          location: res.getHeader('Location') ?? null,
        });
      },
      onError(error) {
        reject(error);
      },
      onSuccess() {
        resolve(upload.url);
      },
      ...options,
    });
    upload.start();
  });
}

function uploadUntilFirstChunk() {
  return new Promise((resolve, reject) => {
    let upload;
    let paused = false;
    upload = new Upload(payload, {
      endpoint,
      chunkSize: 5,
      retryDelays: null,
      storeFingerprintForResuming: false,
      removeFingerprintOnSuccess: true,
      metadata: {
        filename: 'photo.txt',
        filetype: 'text/plain',
      },
      headers: {
        Cookie: cookie,
        'Sec-Fetch-Site': 'same-origin',
      },
      onBeforeRequest(req) {
        requests.push({
          method: req.getMethod(),
          url: req.getURL(),
          offset: req.getHeader('Upload-Offset') ?? null,
        });
      },
      onAfterResponse(req, res) {
        responses.push({
          method: req.getMethod(),
          status: res.getStatus(),
          offset: res.getHeader('Upload-Offset') ?? null,
          location: res.getHeader('Location') ?? null,
        });
      },
      onChunkComplete(_chunkSize, bytesAccepted) {
        if (paused) {
          return;
        }
        paused = true;
        upload.abort().then(
          () => resolve({ uploadUrl: upload.url, bytesAccepted }),
          reject,
        );
      },
      onError(error) {
        if (!paused) {
          reject(error);
        }
      },
      onSuccess() {
        if (!paused) {
          reject(new Error('upload completed before the e2e could pause after the first chunk'));
        }
      },
    });
    upload.start();
  });
}

const paused = await uploadUntilFirstChunk();
const uploadUrl = paused.uploadUrl;

assert.ok(uploadUrl, 'tus-js-client should expose the upload URL');
assert.equal(paused.bytesAccepted, 5, 'expected pause after one accepted chunk');
assert.ok(requests.some((entry) => entry.method === 'POST'), 'expected POST create');
assert.ok(
  requests.filter((entry) => entry.method === 'PATCH').length === 1,
  'expected the first upload to stop after one PATCH',
);
assert.ok(responses.some((entry) => entry.status === 201), 'expected 201 create response');

const resumedUrl = await uploadWithTus({
  endpoint: null,
  uploadUrl,
});

assert.equal(resumedUrl, uploadUrl);
assert.ok(requests.some((entry) => entry.method === 'HEAD'), 'expected HEAD resume check');
assert.ok(
  requests.filter((entry) => entry.method === 'PATCH').length >= 2,
  'expected resumed upload to send remaining PATCH requests',
);
assert.ok(
  responses.some((entry) => entry.status === 204 && entry.offset === String(payload.length)),
  'expected final 204 response with the completed upload offset',
);

const patchCountBeforeResume = requests.filter((entry) => entry.method === 'PATCH').length;
const completedResumeUrl = await uploadWithTus({
  endpoint: null,
  uploadUrl,
});

assert.equal(completedResumeUrl, uploadUrl);
assert.equal(
  requests.filter((entry) => entry.method === 'PATCH').length,
  patchCountBeforeResume,
  'resume against a completed upload should not send another PATCH',
);

console.log(JSON.stringify({ uploadUrl, requests, responses }, null, 2));
