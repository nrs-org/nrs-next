import xml.etree.ElementTree as ET, pathlib, collections, re, csv, sqlite3, json, sys
binds={r['entry_id']:r['url'] for r in csv.DictReader(open('strip/bindings.tsv'),delimiter='\t') if r['url']}
lib=sqlite3.connect('file:musiclib.db?mode=ro',uri=True)
def lib_entry(url):
    r=lib.execute("select entry_id from entry_source where identifier=? limit 1",(url,)).fetchone()
    return r[0] if r else None
# NRS artist entry per musiclib entry (unique only)
per=collections.defaultdict(set)
for e,u in binds.items():
    le=lib_entry(u)
    if le: per[le].add(e)
# roles from legacy XML with $ resolution
roles=[]
def walk(node, enc):
    for c in node:
        if c.tag=='def': continue
        if c.tag=='entry':
            full=c.get('id').replace('$',enc) if enc else c.get('id'); walk(c, full)
        elif c.tag=='role' and enc:
            roles.append((enc, c.get('id').replace('$',enc), c.get('roles')))
for p in pathlib.Path('../nrs-impl/nrs').rglob('*.xml'):
    walk(ET.parse(p).getroot(), None)
def norm_role(r):
    r=r.lower()
    out=set()
    if 'vocal' in r and 'arrang' not in r or r in ('performer',): out.add('vocal')
    if 'arrang' in r or r in ('remixer','remix'): out.add('arrange')
    if 'compos' in r or 'music by' in r or 'written' in r: out.add('compose')
    if 'lyric' in r or 'words' in r: out.add('lyrics')
    if r.startswith('instrument') or 'guitar' in r or 'drum' in r or 'bass' in r or 'piano' in r or r=='programming': out.add('inst')
    if 'produc' in r: out.add('prod')
    if r=='listed_artist': out.add('listed')
    return out or {'other'}
cls=collections.Counter(); agree=collections.Counter(); mism=[]
for t,a,expr in roles:
    tu,au=binds.get(t),binds.get(a)
    tl,al=(lib_entry(tu) if tu else None),(lib_entry(au) if au else None)
    if not tl: cls['track not in musiclib']+=1; continue
    if not al: cls['artist not in musiclib']+=1; continue
    srcs=[(s,i) for s,i in lib.execute("select source,identifier from entry_source where entry_id=?",(tl,))]
    got=set()
    for s,i in srcs:
        for art,role in lib.execute("select artist_identifier,role from contribution where source=? and identifier=?",(s,i)):
            if lib_entry(art)==al: got|=norm_role(role)
    if not got: cls['both present, no credit linking them']+=1; continue
    cls['credited in musiclib']+=1
    atoms={re.sub(r'[*/].*','',x).strip() for x in expr.split('+')}-{'image','image_feat'}
    want={'vocal' if x=='vocal' else x for x in atoms}
    # listed_artist counts as vocal or prod (the credited performer/producer)
    cover=set(got)
    if 'listed' in cover: cover|={'vocal','prod'}
    if 'inst' in want and ('arrange' in cover or 'inst' in cover): cover.add('inst')
    ok=want<=cover
    agree['roles covered' if ok else 'roles differ']+=1
    if not ok and len(mism)<12: mism.append((t,a,expr,sorted(got)))
print('role lines',len(roles))
for k,v in cls.most_common(): print(f'  {k}: {v}')
for k,v in agree.most_common(): print(f'    {k}: {v}')
for m in mism: print('     ',m)

# --- could a default rule regenerate the exact expression? ---
def predict(t,a):
    tl,al=lib_entry(binds.get(t,'')) if binds.get(t) else None, lib_entry(binds.get(a,'')) if binds.get(a) else None
    if not (tl and al): return None
    srcs=[(s,i) for s,i in lib.execute("select source,identifier from entry_source where entry_id=?",(tl,))]
    roles_=set(); feat=False; listed=False; n_listed=set()
    for s,i in srcs:
        rows=lib.execute("select artist_identifier,role,main_artist,extra from contribution where source=? and identifier=? order by id",(s,i)).fetchall()
        # listed artists in credit order; anyone after a "feat." joinphrase is featured
        after_feat=False
        for art,role,main,extra in rows:
            ae=lib_entry(art)
            if role=='listed_artist':
                n_listed.add(ae)
                if ae==al: listed=True; feat=feat or after_feat
                jp=((json.loads(extra) if extra else None) or {}); jp=(jp.get('joinphrase') if isinstance(jp,dict) else '') or ''; jp=jp.lower()
                if 'feat' in jp or 'ft.' in jp or ' with ' in jp and False: after_feat=True
            elif ae==al: roles_|=norm_role(role)
    if not listed and not roles_: return None
    if listed and not (roles_-{'vocal'}):
        return 'vocal+image_feat' if feat else ('vocal+image' if len(n_listed)<=1 else 'vocal+image/%d' % len(n_listed))
    return None
exact=collections.Counter(); byexpr=collections.Counter()
for t,a,expr in roles:
    p=predict(t,a)
    if p is None: exact['no prediction']+=1; continue
    exact['exact' if p==expr else 'different']+=1
    if p!=expr: byexpr[(p,expr)]+=1
print('default-rule regeneration:',dict(exact))
for (p,e),n in byexpr.most_common(8): print(f'   predicted {p!r} but XML has {e!r}: {n}')
