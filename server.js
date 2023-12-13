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
    if (req.headers['content-type']) {
      res.writeHead(200, {
        'content-type': req.headers['content-type'],
      });
    }
    req.pipe(res);
  }
}).listen(8080);
