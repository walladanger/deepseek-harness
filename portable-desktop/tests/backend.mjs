// Real HTTP and descendant-process fixture for Windows launcher ownership checks.
import http from 'node:http';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const mode = process.argv[2];
if (process.env.WRAPPER_TEST_HOME) {
  if (process.env.WRAPPER_TEST_HOME.startsWith('\\\\?\\')) throw new Error('The child received a namespace prefix that breaks source module resolution.');
  mkdirSync(process.env.WRAPPER_TEST_HOME, { recursive: true });
  writeFileSync(`${process.env.WRAPPER_TEST_HOME}/marker.txt`, 'Contained child environment');
}
writeFileSync(`pid-${mode}.txt`, String(process.pid));
if (mode === 'child') {
  setInterval(() => {}, 1000);
} else {
  spawn(process.execPath, [fileURLToPath(import.meta.url), 'child'], { stdio: 'ignore' });
  const waiting = setInterval(() => {
    if (!existsSync('pid-child.txt')) return;
    clearInterval(waiting);
    if (mode === 'early-exit') process.exit(0);
    if (mode === 'timeout') { setInterval(() => {}, 1000); return; }
    const server = http.createServer((request, response) => {
      if (mode === 'denied') { response.writeHead(401).end('Unauthorized'); return; }
      if (request.url === '/?token=fixture-token') {
        response.writeHead(302, { 'Set-Cookie': 'fixture-auth=ok; HttpOnly; Path=/', Location: './' }).end();
      } else if (request.headers.cookie === 'fixture-auth=ok') {
        response.writeHead(200, { 'Content-Type': 'text/html' }).end('<!doctype html><html><body>Original application fixture</body></html>');
      } else response.writeHead(401).end('Unauthorized');
    });
    server.listen(0, '127.0.0.1', () => {
      const url = `http://127.0.0.1:${server.address().port}/?token=fixture-token`;
      process.stdout.write('dsh w');
      setImmediate(() => process.stdout.write(`eb: ${url}\n`));
    });
  }, 10);
}
