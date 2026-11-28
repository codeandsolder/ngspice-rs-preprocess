"""Converged ngspice 47 normal-channel CORSRD=2 circuit reference.

CORSRD=2 leaves the explicit series-node path disabled while retaining the
nonlinear channel correction. Differentiate the converged circuit current,
because external and internal partials share the same physical terminal here.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

CARDS = (
    'rd=0.03 rs=0.03 rd23=0.1 rd21=0.6',
    'rd=0.04 rs=0 rd23=0.04 rd21=0.75 rd20=2 rd24=0.03 rd25=1 rd22=0.003 rd22d=1.2 rd23l=0.03 rd23lp=0.7 rd23s=0.8 rd23sp=0.6',
    'rd=0 rs=0 rd23=0.1 rd21=0.6',
)
HV2_EXTRA = ' ptl=0.02 ptlp=0.7 ptp=3.7 pt2=0.12 pt4=0.4 pt4p=0.8 gdl=0.001 gdld=0.2u gdlp=0.4 gdsleak=1e-7'

def main():
    out = Path(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix='hisim-access-circuit-') as tmp, out.open('w') as f:
        f.write('# family polarity temp_C length_m width_m multiplicity NF config normalized_Vgs Vds Vbs; Ids, finite differences wrt tied Vgs Vds Vbs\n')
        for family, polarity, temp, config in product((1,2), (1,-1), (27.,100.), range(len(CARDS))):
            rows = list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)), (0.,2.,4.,8.), (.01,.1,1.,5.), (0.,-.5)))
            card = f'.model xhv {"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd=2 coisti=0 coisub=0 coadov=0 coovlp=0 conqs=0 '+ CARDS[config]
            if family==2:
                card += ' cordrift=0' + HV2_EXTRA
            deck = ['* wrapper title', '* native access correction', card, f'.temp {temp}']
            h=1e-3
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                for j in range(13):
                    bias=[vg,vd,vb]
                    if j:
                        bias[(j-1)//4] += h * (1.,-1.,2.,-2.)[(j-1)%4]
                    k=i*13+j
                    deck.extend((f'Vd{k} d{k} 0 {polarity*bias[1]}', f'Vg{k} g{k} 0 {polarity*bias[0]}', f'Vb{k} b{k} 0 {polarity*bias[2]}', f'M{k} d{k} g{k} 0 b{k} xhv l={length} w={width} m={m} nf={nf}'))
            deck.extend(('.control','set numdgt=16','op'))
            deck.extend(f'print @m{i}[ids]' for i in range(len(rows)*13))
            deck.extend(('quit','.endc','.end'))
            proc = subprocess.run(['ngspice','-b','/dev/stdin'], input='\n'.join(deck)+'\n', text=True, capture_output=True, cwd=tmp, check=True, timeout=120)
            if 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip():
                print(proc.stderr,file=sys.stderr)
            data = {int(i):float(value) for i,value in re.findall(r'@m(\d+)\[ids\]\s*=\s*([^\s]+)',proc.stdout)}
            assert len(data)==len(rows)*13, (len(data),proc.stdout[-1000:])
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                k=i*13
                values=[data[k],*((8*(data[k+j]-data[k+j+1])-(data[k+j+2]-data[k+j+3]))/(12*h) for j in (1,5,9))]
                assert all(map(math.isfinite,values)), values
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,vg,vd,vb,*values)))+'\n')

if __name__ == '__main__':
    main()
