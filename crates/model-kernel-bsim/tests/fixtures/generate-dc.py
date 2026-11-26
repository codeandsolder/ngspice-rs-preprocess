from itertools import product
from pathlib import Path
import re, subprocess
base=Path('dc-probe.cir').read_text()
rows=['# ngspice 47 OP; pmos Rsh Vd Vg Vs Vb Ivd Ivg Ivs Ivb (supply signs)']
for pmos,rsh,forward,vbs in product([0,1],[0,52],[0,1],[-.4,.04]):
    sign=-1 if pmos else 1
    vd,vs=(1.2,.1) if forward else (.1,1.2)
    vg=1.7; vb=.1+vbs
    volts=[sign*x for x in (vd,vg,vs,vb)]
    deck=base.replace('nmos(',('pmos' if pmos else 'nmos')+'(').replace('rsh=52',f'rsh={rsh}').replace('display\nprint all','print i(vd) i(vg) i(vs) i(vb)')
    for name,value in zip('dgsb',volts):
        deck=re.sub(rf'^v{name} {name} 0 .*$',f'v{name} {name} 0 {value:.17g}',deck,flags=re.M)
    f=Path('dc-case.cir');f.write_text(deck)
    result=subprocess.run(['ngspice','-b',str(f)],capture_output=True,text=True,check=True)
    if 'ngspice-47 done' not in result.stdout: raise ValueError(result.stdout)
    values=[float(re.search(rf'i\(v{node}\)\s*=\s*([-+0-9.eE]+)',result.stdout)[1]) for node in 'dgsb']
    rows.append(' '.join(str(x) for x in [pmos,rsh,*volts,*values]))
Path('bsim1-dc-ngspice47.txt').write_text('\n'.join(rows)+'\n')
print(Path('bsim1-dc-ngspice47.txt').read_text())
