# python3 -I measure.py <out dir> [-v]: sizes of what Claude Code's first request carries. Tokens are estimated as
# characters / 4 (good enough to compare; not exact).
import json, sys, glob
f = sys.argv[1] + '/req-1.json'
j = json.load(open(f))
sysp = j.get('system')
sys_txt = ''.join(b.get('text','') for b in sysp) if isinstance(sysp, list) else (sysp or '')
tools = j.get('tools', [])
mcp = [t for t in tools if t.get('name','').startswith('mcp__')]
msgs = json.dumps(j.get('messages', []))
t = lambda s: len(s)//4
tot = t(sys_txt)+t(json.dumps(tools))+t(msgs)
print(f"{sys.argv[1].split('/')[-1]:>8}: system {t(sys_txt):>5} | tools {len(tools):>2} = {t(json.dumps(tools)):>5} (browser {len(mcp)} = {t(json.dumps(mcp))}) | messages {t(msgs):>5} | total ~{tot}")
if '-v' in sys.argv: print(sorted(x['name'] for x in tools if not x['name'].startswith('mcp__')))
