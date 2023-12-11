'use strict';

const http = require('http');
const { pipeline } = require('node:stream/promises');

http.createServer((req, res) => {
  console.log(req.url, req.headers);
  if (req.url === '/stream') {
    fetch('http://httpbin.org/stream-bytes/5000000')
      .then((r) => {
        res.writeHead(r.status, Object.fromEntries([...r.headers.entries()]));
        return r.body;
      })
      .then((body) => pipeline(body, res));
  } else if (req.method === 'GET') {
    res.end('hi!');
  } else {
    req.pipe(res);
  }
}).listen(8080);
