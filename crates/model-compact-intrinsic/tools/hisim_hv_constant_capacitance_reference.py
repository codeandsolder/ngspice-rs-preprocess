"""Measure constant overlap and fringing by subtracting paired AC gate excitations."""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

CARDS=(
    'cgdo=4e-10 cgso=2e-10 cgbo=3e-10 tpoly=0',
    'cgbo=0 tpoly=0',
    'cgdo=0 cgso=0 cgbo=0 tpoly=150e-9',
    'cgdo=4e-10 cgso=2e-10 cgbo=3e-10 tpoly=150e-9',
)
def main():
    with tempfile.TemporaryDirectory(prefix='hisim-constant-') as tmp, Path(sys.argv[1]).open('w') as f:
        f.write('# family polarity temp length width M NF config reverse Vgs Vds Vbs; delta capacitance from gate excitation (D,S,B,G)\n')
        for family,polarity,temp,config in product((1,2),(1,-1),(27.,100.),range(len(CARDS))):
            rows=list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)),(False,True),(-2.,1.,5.),(0.1,5.),(0.,-.5)))
            model=f'{"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=0 coisti=0 coisub=0 coadov=1 coovlp=0 coovlps=0 conqs=0 coiigs=0 cogidl=0 xqy=0 xqy1=0 loverld=5e-7 lovers=3e-7'
            if family==2:
                model+=' cordrift=0'
            deck=['* wrapper title','* paired AC constant charge',f'.model baseline {model} cgdo=0 cgso=0 cgbo=0 tpoly=0',f'.model active {model} {CARDS[config]}',f'.temp {temp}']
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                dv,gv,bv=(-vd,vg-vd,vb-vd) if reverse else (vd,vg,vb)
                for k,card in enumerate(('baseline','active')):
                    j=2*i+k
                    deck.extend((f'Vd{j} d{j} 0 {polarity*dv}',f'Vg{j} g{j} 0 {polarity*gv} ac 1',
                                 f'Vb{j} b{j} 0 {polarity*bv}',f'Vs{j} s{j} 0 0',
                                 f'M{j} d{j} g{j} s{j} b{j} {card} l={length} w={width} m={m} nf={nf}'))
            deck.extend(('.control','set numdgt=16','ac lin 1 1e6 1e6'))
            for j in range(2*len(rows)):
                for term in ('d','s','b','g'):
                    deck.append(f'let cap{term}{j}=imag(i(v{term}{j}))/(2*pi*frequency)')
                    deck.append(f'print cap{term}{j}')
            deck.extend(('quit','.endc','.end'))
            proc=subprocess.run(['ngspice','-b','/dev/stdin'],input='\n'.join(deck)+'\n',text=True,capture_output=True,cwd=tmp,timeout=120)
            if proc.returncode or 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip(): print(proc.stderr,file=sys.stderr)
            values={(int(j),term):float(value.split(",")[0]) for term,j,value in re.findall(r'cap([dsbg])(\d+)\s*=\s*([^\s]+)',proc.stdout)}
            assert len(values)==len(rows)*8,(len(values),proc.stdout[-1000:],proc.stderr[-1000:])
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                cap=[values[2*i+1,n]-values[2*i,n] for n in ('d','s','b','g')]
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,int(reverse),vg,vd,vb,*cap)))+'\n')

if __name__=='__main__':
    main()
