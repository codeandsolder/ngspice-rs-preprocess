"""Converged ngspice 47 normal-channel access and STI circuit references.

CORSRD=2 leaves the explicit series-node path disabled while retaining the
nonlinear channel correction. Differentiate the converged circuit current,
because external and internal partials share the same physical terminal here.
Pass --sti for STI edge current or --impact for channel impact ionization.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

ACCESS_CARDS = (
    'rd=0.03 rs=0.03 rd23=0.1 rd21=0.6',
    'rd=0.04 rs=0 rd23=0.04 rd21=0.75 rd20=2 rd24=0.03 rd25=1 rd22=0.003 rd22d=1.2 rd23l=0.03 rd23lp=0.7 rd23s=0.8 rd23sp=0.6',
    'rd=0 rs=0 rd23=0.1 rd21=0.6',
)
STI_CARDS = (
    'wsti=0.1u nsti=1e17 vthsti=0.1',
    'wsti=0.35u nsti=5e17 vthsti=-0.2 vdsti=0.02 scsti1=0.1 scsti2=0.2 wstil=0.5 wstiw=0.3',
    'wsti=0.05u nsti=1e18 vthsti=0.2 vdsti=-0.01 scsti1=0.3 scsti2=0.1',
)
IMPACT_CARDS = (
    'sub1=0.1 sub2=12 svgs=0.5 svbs=0.5 svds=0.9 slg=0.05u subld1=0 ibpc1=0',
    'sub1=0.2 sub2=8 svgs=0.7 svbs=0.3 svds=0.85 slg=0.1u svgsl=1e-5 svgsw=1e-4 svbsl=1e-5 slgl=1e-5 sub1l=1e-5 sub2l=1e-5 subld1=0 ibpc1=0',
    'sub1=0 sub2=8 subld1=0 ibpc1=0',
)
HV2_EXTRA = ' ptl=0.02 ptlp=0.7 ptp=3.7 pt2=0.12 pt4=0.4 pt4p=0.8 gdl=0.001 gdld=0.2u gdlp=0.4 gdsleak=1e-7'

def main():
    out = Path(sys.argv[1])
    sti = "--sti" in sys.argv[2:]
    impact = "--impact" in sys.argv[2:]
    cards = IMPACT_CARDS if impact else STI_CARDS if sti else ACCESS_CARDS
    gate_grid = (0.,2.,3.,5.,8.) if sti or impact else (0.,2.,4.,8.)
    drain_grid = (.1,1.,5.,15.) if impact else (.01,.1,1.,5.)
    observable = "isub" if impact else "ids"
    with tempfile.TemporaryDirectory(prefix='hisim-current-circuit-') as tmp, out.open('w') as f:
        f.write('# family polarity temp_C length_m width_m multiplicity NF config normalized_Vgs Vds Vbs; selected current, finite differences wrt tied Vgs Vds Vbs\n')
        for family, polarity, temp, config in product((1,2), (1,-1), (27.,100.), range(len(cards))):
            rows = list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)), gate_grid, drain_grid, (0.,-.5)))
            card = f'.model xhv {"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd={0 if sti or impact else 2} coisti={int(sti)} coisub={int(impact)} coadov=0 coovlp=0 conqs=0 '+ cards[config]
            if family==2:
                card += ' cordrift=0' + ('' if sti or impact else HV2_EXTRA)
            deck = ['* wrapper title', '* native channel current correction', card, f'.temp {temp}']
            h=1e-3
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                for j in range(13):
                    bias=[vg,vd,vb]
                    if j:
                        bias[(j-1)//4] += h * (1.,-1.,2.,-2.)[(j-1)%4]
                    k=i*13+j
                    deck.extend((f'Vd{k} d{k} 0 {polarity*bias[1]}', f'Vg{k} g{k} 0 {polarity*bias[0]}', f'Vb{k} b{k} 0 {polarity*bias[2]}', f'M{k} d{k} g{k} 0 b{k} xhv l={length} w={width} m={m} nf={nf}'))
            deck.extend(('.control','set numdgt=16','op'))
            deck.extend(f'print @m{i}[{observable}]' for i in range(len(rows)*13))
            deck.extend(('quit','.endc','.end'))
            proc = subprocess.run(['ngspice','-b','/dev/stdin'], input='\n'.join(deck)+'\n', text=True, capture_output=True, cwd=tmp, check=True, timeout=120)
            if 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip():
                print(proc.stderr,file=sys.stderr)
            data = {int(i):float(value) for i,value in re.findall(rf'@m(\d+)\[{observable}\]\s*=\s*([^\s]+)',proc.stdout)}
            assert len(data)==len(rows)*13, (len(data),proc.stdout[-1000:])
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                k=i*13
                def slope(j):
                    plus, minus, plus2, minus2 = (data[k+j+i] for i in range(4))
                    # HV1 disables impact ionization in D1 although the
                    # channel has a nonzero diffusion current. At a zero
                    # base point, if either pair of same-side samples is
                    # also identically zero, use the local zero branch;
                    # a centered stencil across the discontinuity is invalid.
                    if impact and family == 1 and data[k] == 0. and ((plus == plus2 == 0.) or (minus == minus2 == 0.)):
                        return 0.
                    return (8*(plus-minus)-(plus2-minus2))/(12*h)
                values=[data[k],*(slope(j) for j in (1,5,9))]
                assert all(map(math.isfinite,values)), values
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,vg,vd,vb,*values)))+'\n')

if __name__ == '__main__':
    main()
