from pathlib import Path
import subprocess
import math
cases=[
("baseline",340,"nmos=1","l=5e-6 w=5e-6"),
("cold",245,"pmos=1 tnom=35 u0=350 vth0=-0.6 k1=0.4 k2=-0.02 rsh=50 rbsh=12 rbody=500 rth0=0.02 cth0=5e-6 csdesw=3e-10 isdif=2e-7 istun=1e-9","l=2e-6 w=3e-6 pd=8e-6 ps=2e-6 nrd=2 nrs=3 nrb=4 m=3"),
("hot",390,"nmos=1 tnom=20 gamma1=0.6 vbm=-4 nsub=2e17 xdif=3.2 isdif=5e-8 istun=3e-10 xtun=1.5 rdsw=100 prt=20 ua=2e-9","l=3e-6 w=7e-6"),
("bins",310,"nmos=1 nch=2e17 lnch=1e16 lint=1e-7 wint=2e-7 dlc=1.5e-7 dwc=2.5e-7 vth0=0.6 lvth0=0.05 wvth0=0.01 pvth0=0.03 ua=3e-9 lua=2e-10 rdsw=100 wr=0.8 cgdo=1e-10 cgso=2e-10 cgeo=3e-10 csdesw=3e-10 vsdfb=-0.2 vsdth=0.5 csdmin=1e-4","l=2e-6 w=4e-6 pd=8e-6 ps=10e-6"),
("one_k",300.15,"pmos=1 k1=0.6 nsub=-6e16 vth0=-0.5","l=1e-6 w=2e-6"),
("paramchk",320,"nmos=1 paramchk=1 rdsw=-1 cgdo=-1e-10 cgso=-1e-10","l=2e-6 w=4e-6"),
]
# No model derivation in this script; collect original C routine outputs verbatim.
lines=[]
for f in ['fd','dd','pd']:
    for name,temp,model,instance in cases:
        if f=='pd' and name in ('cold','bins'):
            model+=' wth0=1e-7 rhalo=2e8 k1w1=2e-7 k1w2=1e-7 ahli=0.03 toxqm=2e-9'
            instance+=' nseg=2 nbc=1 pdbcp=1e-7 psbcp=3e-7 frbody=0.7'
        if f=='pd' and name=='hot': model+=' nrecf0=1.4'
        result=subprocess.run([str(Path(f).resolve()),str(temp),*('m:'+x for x in model.split()),*('i:'+x for x in instance.split())],capture_output=True,text=True)
        Path(f'{f}-{name}.log').write_text(result.stdout+result.stderr)
        if result.returncode: raise RuntimeError((f,name,result.returncode,result.stderr))
        values=[line.split()[1:] for line in result.stdout.splitlines() if line.startswith('value ')]
        if not all(math.isfinite(float(v)) for _,v in values): raise RuntimeError((f,name,values))
        lines.append(f'case\t{f}\t{name}\t{temp}\t{model}\t{instance}')
        lines.extend(f'{k}\t{v}' for k,v in values)
Path('thermal.tsv').write_text('\n'.join(lines)+'\n')
print(len(cases)*3,'cases',sum(line.startswith('case\t')==False for line in lines),'values')
