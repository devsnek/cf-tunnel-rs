'use strict';

const { WebSocketServer } = require('ws');
const http = require('http');
const { pipeline } = require('node:stream/promises');

const server = http.createServer((req, res) => {
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
});

const wss = new WebSocketServer({ server });

wss.on('connection', (ws, req) => {
  console.log(req.url, req.headers);
  ws.on('message', (data) => {
    ws.send(data);
  });
});

server.listen(8080);

/*
require('net').createServer((c) => {
  c.setEncoding('utf8');
  c.on('data', console.log);
}).listen(8080);
*/
