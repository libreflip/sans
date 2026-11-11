import json, sys, itertools
p = sys.argv[1]
flat = json.load(open(p))

def crc16(data, init, poly=0x3d65):
    rem = init
    for b in data:
        rem ^= b << 8
        for _ in range(8):
            rem = ((rem << 1) ^ poly) & 0xffff if rem & 0x8000 else (rem << 1) & 0xffff
    return rem

# build tree preserving id order
class Node(dict): pass
root = {"members": {}, "minid": 0}
for path, e in flat.items():
    parts = path.split(".") if path else [""]
    cur = root
    for part in parts[:-1]:
        cur = cur["members"].setdefault(part, {"members": {}, "obj": True, "name": part})
    cur["members"][parts[-1]] = {"leaf": e, "name": parts[-1]}

def minid(n):
    if "leaf" in n: return n["leaf"]["id"]
    return min(minid(m) for m in n["members"].values())

def prop(e, order):
    fields = {"name": '"name":"%s"' % e["name"], "id": '"id":%d' % e["id"],
              "type": '"type":"%s"' % e["type"], "access": '"access":"%s"' % e["access"]}
    return "{" + ",".join(fields[k] for k in order) + "}"

def emit(n, v):
    if "leaf" in n:
        e = n["leaf"]
        if e["type"] == "function":
            ins = ",".join(prop(a, v["prop"]) for a in e["inputs"])
            outs = ",".join(prop(a, v["prop"]) for a in e["outputs"])
            if v["fn"] == 0:
                return '{"name":"%s","id":%d,"type":"function","inputs":[%s],"outputs":[%s]}' % (e["name"], e["id"], ins, outs)
            else:
                return '{"name":"%s","id":%d,"type":"function","arguments":[%s]}' % (e["name"], e["id"], ins)
        return prop(e, v["prop"])
    ms = sorted(n["members"].values(), key=minid)
    return '{"name":"%s","type":"object","members":[%s]}' % (n["name"], ",".join(emit(m, v) for m in ms))

def whole(v):
    ms = sorted(root["members"].values(), key=minid)
    return "[" + ",".join(emit(m, v) for m in ms) + "]"

target = 39744
matched = False
for fn in (0,):
    for order in (["name","id","type","access"],):
        s = whole({"prop": order, "fn": fn}).encode()
        for init in (1, 0x1337, 0):
            for tail in (b"", b"\0"):
                c = crc16(s + tail, init)
                matched |= c == target
                print(fn, order, init, tail, len(s), c, c == target)
if not matched:
    sys.exit("no reconstruction matches descriptor CRC %d; recon.json not written" % target)
open("recon.json","wb").write(whole({"prop":["name","id","type","access"],"fn":0}).encode())
print(whole({"prop":["name","id","type","access"],"fn":0})[:400])
