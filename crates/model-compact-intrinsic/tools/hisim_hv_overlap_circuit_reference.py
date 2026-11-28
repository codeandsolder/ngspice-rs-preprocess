"""Native HiSIM-HV intrinsic overlap charge measured by paired identical OP decks."""
from itertools import product
from pathlib import Path
import re
import subprocess
import sys
import tempfile

CARDS=(
    'coovlp=1 coovlps=0 nover=0',
    'coovlp=0 coovlps=1 novers=0',
    'coovlp=1 coovlps=1 nover=1e17 novers=5e17 coqovsm=0',
    'coovlp=1 coovlps=1 nover=1e17 novers=5e17 coqovsm=1',
    'coovlp=1 coovlps=1 nover=1e17 novers=5e17 coqovsm=2',
    'coovlp=1 coovlps=1 nover=2e17 novers=3e17 coqovsm=1 cvdsover=0.3 qovadd=0.001',
)

def main():
    reverse = "--reverse" in sys.argv[2:]
    with tempfile.TemporaryDirectory(prefix='hisim-overlap-') as tmp, Path(sys.argv[1]).open('w') as f:
        f.write('# family polarity temp length width M NF config Vgs Vds Vbs; delta qg qd qb\n')
        for family,polarity,temp,config in product((1,2),(1,-1),(27.,100.),range(len(CARDS))):
            gates=(-2.,0.,1.,3.,8.) if reverse else (-2.,0.,1.,2.,3.,5.,8.)
            drains=(0.01,1.,5.,15.) if reverse else (0.,0.01,1.,5.,15.)
            rows=list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)),gates,drains,(0.,-.5)))
            model=f'{"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=0 coisti=0 coisub=0 coadov=1 conqs=0 coiigs=0 cogidl=0 xqy=0 xqy1=0 cgdo=0 cgso=0 cgbo=0 tpoly=0 loverld=5e-7 lovers=3e-7'
            if family==2:
                model+=' cordrift=0'
            card=CARDS[config] if family==2 else CARDS[config].replace(' qovadd=0.001','')
            deck=['* wrapper title','* paired overlap charge',f'.model baseline {model} coovlp=0 coovlps=0',f'.model active {model} {card}',f'.temp {temp}']
            for i,((length,width,m,nf),vg,vd,vb) in enumerate(rows):
                dv,gv,bv=(-vd,vg-vd,vb-vd) if reverse else (vd,vg,vb)
                deck.extend((f'Vd{i} d{i} 0 {polarity*dv}',f'Vg{i} g{i} 0 {polarity*gv}',f'Vb{i} b{i} 0 {polarity*bv}'))
                for k,card in enumerate(('baseline','active')):
                    deck.append(f'M{2*i+k} d{i} g{i} 0 b{i} {card} l={length} w={width} m={m} nf={nf}')
            deck.extend(('.control','set numdgt=16','op'))
            deck.extend('print '+' '.join(f'@m{i}[{name}]' for name in ('qg','qd','qb')) for i in range(2*len(rows)))
            deck.extend(('quit','.endc','.end'))
            proc=subprocess.run(['ngspice','-b','/dev/stdin'],input='\n'.join(deck)+'\n',text=True,capture_output=True,cwd=tmp,timeout=120)
            if proc.returncode or 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip(): print(proc.stderr,file=sys.stderr)
            values={(int(i),name):float(value) for i,name,value in re.findall(r'@m(\d+)\[(\w+)\]\s*=\s*([^\s]+)',proc.stdout)}
            assert len(values)==len(rows)*6
            for i,((length,width,m,nf),vg,vd,vb) in enumerate(rows):
                charge=[values[2*i+1,n]-values[2*i,n] for n in ('qg','qd','qb')]
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,vg,vd,vb,*charge)))+'\n')

if __name__=='__main__':
    main()
