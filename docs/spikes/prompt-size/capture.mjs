// A stand-in for Anthropic's API that records what Claude Code sends, so the
// size of an agent's starting prompt can be measured without an account.
// See docs/agents/lean-prompts.md for how to run it.
//
// node capture.mjs <out dir> <port>: each /v1/messages request body goes to
// <out dir>/req-N.json. The first answer asks for one Bash call (`echo hi`),
// the next ones say DONE. Each answer reports 100 fresh input tokens,
// FAKE_READ (default 1000) cache reads and 50 cache writes, so a big
// FAKE_READ makes Claude Code think its conversation is that full: that is
// how the compaction thresholds were found.
import http from 'node:http';
import fs from 'node:fs';
const out = process.argv[2]; const port = +process.argv[3];
let n = 0;
http.createServer((req, res) => {
  let body = '';
  req.on('data', c => body += c);
  req.on('end', () => {
    if (!req.url.includes('/v1/messages')) { res.writeHead(404); return res.end('{}'); }
    if (req.url.includes('count_tokens')) { res.writeHead(200, {'content-type':'application/json'}); return res.end('{"input_tokens":1}'); }
    fs.writeFileSync(`${out}/req-${++n}.json`, body);
    const j = JSON.parse(body);
    res.writeHead(200, {'content-type': 'text/event-stream'});
    const ev = (t, d) => res.write(`event: ${t}\ndata: ${JSON.stringify(d)}\n\n`);
    ev('message_start', {type:'message_start', message:{id:'msg_'+n, type:'message', role:'assistant', model:j.model, content:[], stop_reason:null, usage:{input_tokens:100, cache_read_input_tokens:+(process.env.FAKE_READ||1000), cache_creation_input_tokens:50, output_tokens:1}}});
    const main = (j.tools||[]).some(t => t.name === 'Bash');
    if (n === 1 && main) {
      ev('content_block_start', {type:'content_block_start', index:0, content_block:{type:'tool_use', id:'toolu_1', name:'Bash', input:{}}});
      ev('content_block_delta', {type:'content_block_delta', index:0, delta:{type:'input_json_delta', partial_json:JSON.stringify({command:'echo hi', description:'Say hi'})}});
      ev('content_block_stop', {type:'content_block_stop', index:0});
      ev('message_delta', {type:'message_delta', delta:{stop_reason:'tool_use'}, usage:{output_tokens:10}});
    } else {
      ev('content_block_start', {type:'content_block_start', index:0, content_block:{type:'text', text:''}});
      ev('content_block_delta', {type:'content_block_delta', index:0, delta:{type:'text_delta', text:'DONE'}});
      ev('content_block_stop', {type:'content_block_stop', index:0});
      ev('message_delta', {type:'message_delta', delta:{stop_reason:'end_turn'}, usage:{output_tokens:10}});
    }
    ev('message_stop', {type:'message_stop'});
    res.end();
  });
}).listen(port, '127.0.0.1');
