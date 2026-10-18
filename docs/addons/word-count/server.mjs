// A tiny MCP server for the Word count example add-on, with no
// dependencies, so the whole add-on is these two files.
//
// Decurion starts it in a sealed box of its own beside the agent's box,
// with this folder at /addon (read-only) and the port in DECURION_MCP_PORT.
// It must listen on 127.0.0.1 and answer MCP over HTTP at /mcp. It answers
// each request with plain JSON, which MCP's streamable HTTP allows, and
// keeps no sessions.

import http from 'node:http';

const PORT = Number(process.env.DECURION_MCP_PORT ?? 8940);
const HOSTS = new Set([`127.0.0.1:${PORT}`, `localhost:${PORT}`]);
const MAX_BODY = 1 << 20;

const TOOLS = [
  {
    name: 'count_words',
    description: 'Count the words in a piece of text.',
    inputSchema: {
      type: 'object',
      properties: { text: { type: 'string', description: 'The text to count.' } },
      required: ['text'],
    },
  },
];

function answer(msg) {
  const { id, method, params } = msg;
  switch (method) {
    case 'initialize':
      return {
        protocolVersion: params?.protocolVersion ?? '2025-06-18',
        capabilities: { tools: {} },
        serverInfo: { name: 'word-count', version: '0.1.0' },
      };
    case 'ping':
      return {};
    case 'tools/list':
      return { tools: TOOLS };
    case 'tools/call': {
      if (params?.name !== 'count_words') throw { code: -32602, message: `no tool ${params?.name}` };
      const text = String(params?.arguments?.text ?? '');
      const n = text.split(/\s+/).filter(Boolean).length;
      return { content: [{ type: 'text', text: `${n} ${n === 1 ? 'word' : 'words'}` }] };
    }
    default:
      throw { code: -32601, message: `no method ${method} (id ${id})` };
  }
}

function reply(msg) {
  // A notification gets no answer.
  if (msg?.id === undefined || msg.id === null) return null;
  try {
    return { jsonrpc: '2.0', id: msg.id, result: answer(msg) };
  } catch (e) {
    return { jsonrpc: '2.0', id: msg.id, error: { code: e.code ?? -32603, message: e.message ?? String(e) } };
  }
}

const server = http.createServer((req, res) => {
  // Only callers on this box's own localhost, by name, so a web page cannot
  // reach the tool by pointing a name of its own at this address.
  if (!HOSTS.has(req.headers.host ?? '') || new URL(req.url, 'http://x').pathname !== '/mcp') {
    res.writeHead(404).end();
    return;
  }
  if (req.method !== 'POST') {
    res.writeHead(405, { allow: 'POST' }).end();
    return;
  }
  let body = '';
  req.setEncoding('utf8');
  req.on('data', (c) => {
    body += c;
    if (body.length > MAX_BODY) req.destroy();
  });
  req.on('end', () => {
    let msg;
    try {
      msg = JSON.parse(body);
    } catch {
      res.writeHead(400, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'not JSON' } }));
      return;
    }
    const out = Array.isArray(msg) ? msg.map(reply).filter(Boolean) : reply(msg);
    if (out === null || (Array.isArray(out) && out.length === 0)) {
      res.writeHead(202).end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(out));
  });
});

server.listen(PORT, '127.0.0.1', () => console.error(`word-count listening on 127.0.0.1:${PORT}`));
