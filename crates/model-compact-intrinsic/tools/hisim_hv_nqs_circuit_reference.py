"""Paired native QS/NQS AC circuit response at a clamped physical device."""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

CARDS=(
    'dly1=100e-12 dly2=0.7 dly3=0.8e-6',
    'dly1=300e-12 dly2=0.2 dly3=2e-6',
)
FREQS=(1e8,1e9,1e10)
def main():
    with tempfile.TemporaryDirectory(prefix='hisim-nqs-') as tmp, Path(sys.argv[1]).open('w') as f:
        f.write('# family polarity temp length width M NF config reverse excitation Vgs Vds Vbs frequency; NQS-QS source currents real/imag (D,S,B,G)\n')
        for family,polarity,temp,config,excite in product((1,2),(1,-1),(27.,100.),range(len(CARDS)),('g','d','b')):
            rows=list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)),(False,True),(-2.,1.,3.,5.,8.),(0.1,5.),(0.,-.5)))
            model=f'{"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=0 coisti=0 coisub=0 coadov=0 coovlp=0 coovlps=0 coiigs=0 cogidl=0 xqy=0 xqy1=0 {CARDS[config]}'
            if family==2:
                model+=' cordrift=0'
            deck=['* wrapper title','* paired AC NQS',f'.model baseline {model} conqs=0',f'.model active {model} conqs=1',f'.temp {temp}']
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                dv,gv,bv=(-vd,vg-vd,vb-vd) if reverse else (vd,vg,vb)
                for k,card in enumerate(('baseline','active')):
                    j=2*i+k
                    for term,voltage in zip(('d','g','b','s'),(dv,gv,bv,0.)):
                        ac=' ac 1' if term==excite else ''
                        deck.append(f'V{term}{j} {term}{j} 0 {polarity*voltage}{ac}')
                    deck.append(f'M{j} d{j} g{j} s{j} b{j} {card} l={length} w={width} m={m} nf={nf}')
            deck.extend(('.control','set numdgt=16'))
            for z,freq in enumerate(FREQS):
                deck.append(f'ac lin 1 {freq} {freq}')
                for j in range(2*len(rows)):
                    for term in ('d','s','b','g'):
                        deck.append(f'let cap{z}{term}{j}=i(v{term}{j})')
                        deck.append(f'print cap{z}{term}{j}')
            deck.extend(('quit','.endc','.end'))
            proc=subprocess.run(['ngspice','-b','/dev/stdin'],input='\n'.join(deck)+'\n',text=True,capture_output=True,cwd=tmp,timeout=120)
            if proc.returncode or 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip(): print(proc.stderr,file=sys.stderr)
            values={(int(z),int(j),term):complex(*map(float,value.split(','))) for z,term,j,value in re.findall(r'cap(\d)([dsbg])(\d+)\s*=\s*([^\s]+)',proc.stdout)}
            assert len(values)==len(rows)*8*len(FREQS),(len(values),proc.stdout[-1000:],proc.stderr[-1000:])
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                for z,freq in enumerate(FREQS):
                    cap=[values[z,2*i+1,n]-values[z,2*i,n] for n in ('d','s','b','g')]
                    f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,int(reverse),('g','d','b').index(excite),vg,vd,vb,freq,*(v for c in cap for v in (c.real,c.imag)))))+'\n')

if __name__=='__main__':
    main()
