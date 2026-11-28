"""Native HiSIM-HV source-resolved noise via a unit drain transresistance."""
from itertools import product
from pathlib import Path
import math
import subprocess
import sys
import tempfile

CARDS=(
    'coflick=1 cothrml=1 coign=1',
    'coflick=1 cothrml=1 coign=1 nfalp=2e-6 nftrp=2e10 falph=0.85 cit=1e-8 bb=1.5',
    'coflick=0 cothrml=1 coign=0',
    'coflick=1 cothrml=0 coign=1',
)
FREQS=(1e3,1e6,1e9)
def main():
    with tempfile.TemporaryDirectory(prefix='hisim-hv-noise-') as tmp, Path(sys.argv[1]).open('w') as f:
        f.write('# family polarity temp length width M NF config reverse Vgs Vds Vbs frequency; thermal flicker induced PSD A^2/Hz\n')
        p=Path(tmp)
        for family,polarity,temp,geometry,config,reverse,gate,drain,body in product(
            (1,2),(1,-1),(27.,100.),((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)),range(len(CARDS)),(False,True),(1.,3.,5.,8.),(.1,5.),(0.,-.5)):
            length,width,m,nf=geometry
            vd,vg,vb=(-drain,gate-drain,body-drain) if reverse else (drain,gate,body)
            model=f'{"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=0 coisti=0 coisub=0 coadov=0 coovlp=0 coovlps=0 coiigs=0 cogidl=0 xqy=0 xqy1=0 conqs=0 noise=1 {CARDS[config]}'
            if family==2:
                model+=' cordrift=0'
            deck=f"""HiSIM-HV source noise reference
Vd d 0 {polarity*vd}
Vg g 0 {polarity*vg} ac 1
Vb b 0 {polarity*vb}
Vs s 0 0
Hd od 0 Vd 1
M1 d g s b active l={length} w={width} m={m} nf={nf}
.model active {model}
.temp {temp}
.options gmin=1e-18 reltol=1e-10 abstol=1e-18 vntol=1e-12
.control
set numdgt=17
set sqrnoise
set wr_singlescale
noise v(od) Vg dec 1 1k 1g 1
setplot noise1
wrdata d.dat onoise.m1.id onoise.m1.1ovf onoise.m1.ign
quit
.endc
.end
"""
            data=p/'d.dat'
            data.unlink(missing_ok=True)
            proc=subprocess.run(['ngspice','-b','-n','/dev/stdin'],input='wrapper title\n'+deck,text=True,capture_output=True,cwd=tmp,timeout=30)
            if proc.returncode or not data.exists():
                raise RuntimeError(proc.stdout+proc.stderr+deck)
            if proc.stderr.strip(): print(proc.stderr,file=sys.stderr)
            rows=[tuple(map(float,line.split())) for line in data.read_text().splitlines()]
            assert rows and all(len(row)==4 for row in rows), rows
            for freq,thermal,flicker,induced in rows:
                if any(math.isclose(freq, target, rel_tol=1e-12) for target in FREQS):
                    f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,int(reverse),gate,drain,body,freq,thermal,flicker,induced)))+'\n')
if __name__=='__main__':
    main()
