"""Converged ngspice 47 impact/drift/IBPC reference with internal drain nodes.

HV1 reports channel+drift substrate current and channel+IBPC drain current.
HV2 reports all four currents separately. The explicit series-node path
produces a physical nonzero drain-to-prime voltage in HV2; save the hidden
node explicitly (save all suppresses the internal node).
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

CARDS = (
    'sub1=0.1 sub2=12 svgs=0.5 svbs=0.5 svds=0.9 subld1=0 ibpc1=1.5 ibpc2=0.3',
    'sub1=0 subld1=0.04 subld2=2000 ibpc1=2 ibpc2=0.2',
    'sub1=0.2 sub2=8 svgs=0.7 svbs=0.3 svds=0.85 subld1=0.06 subld2=4000 ibpc1=3 ibpc2=0.4',
)

def main():
    out = Path(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix='hisim-impact-drift-') as tmp, out.open('w') as f:
        f.write('# family polarity temp_C length_m width_m multiplicity NF config normalized_Vgs external_Vds Vbs intrinsic_Vds native_ids native_isub native_isubld native_idsibpc (last two HV2 only)\n')
        for family, polarity, temp, config in product((1,2), (1,-1), (27.,100.), range(len(CARDS))):
            rows=list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)), (2.,3.,5.,8.), (.1,1.,5.,15.), (0.,-.5)))
            model=f'.model xhv {"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} coisti=0 coisub=1 coadov=0 coovlp=0 conqs=0 '+CARDS[config]
            if family==1:
                model+=' corsrd=0 coldrift=0'
            else:
                model+=' corsrd=1 cordrift=0 rd=0.002 rs=0 rdvd=0'
                if config==2:
                    model+=' xpdv=2e5 xldld=1u xpvdth=0.2 xpvdthg=0.1 subld1l=0.3 subld1lp=0.8 ibpc1l=0.5 ibpc1lp=0.7'
            deck=['* wrapper title','* drift impact and IBPC',model,f'.temp {temp}']
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                deck.extend((f'Vd{i} d{i} 0 {polarity*vd}', f'Vg{i} g{i} 0 {polarity*vg}', f'Vb{i} b{i} 0 {polarity*vb}', f'M{i} d{i} g{i} 0 b{i} xhv l={length} w={width} m={m} nf={nf}'))
            deck.extend(('.control','set numdgt=16'))
            if family==2:
                deck.append('save '+' '.join(f'v(m{i}#drain)' for i in range(len(rows))))
            deck.append('op')
            observables=('ids','isub') if family==1 else ('ids','isub','isubld','idsibpc')
            for i in range(len(rows)):
                deck.append('print '+' '.join(f'@m{i}[{name}]' for name in observables))
                if family==2:
                    deck.append(f'print v(m{i}#drain)')
            deck.extend(('quit','.endc','.end'))
            proc=subprocess.run(['ngspice','-b','/dev/stdin'],input='\n'.join(deck)+'\n',text=True,capture_output=True,cwd=tmp,check=False,timeout=120)
            if proc.returncode:
                raise RuntimeError(proc.stdout+proc.stderr)
            if 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip():
                print(proc.stderr,file=sys.stderr)
            data={(int(i),name):float(value) for i,name,value in re.findall(r'@m(\d+)\[(\w+)\]\s*=\s*([^\s]+)',proc.stdout)}
            inner={int(i):polarity*float(value) for i,value in re.findall(r'v\(m(\d+)#drain\)\s*=\s*([^\s]+)',proc.stdout)}
            assert len(data)==len(rows)*len(observables),(len(data),proc.stdout[-1000:])
            if family==2:
                assert len(inner)==len(rows),(len(inner),proc.stdout[-1000:])
            for i, ((length,width,m,nf), vg, vd, vb) in enumerate(rows):
                values=[data.get((i,name),0.) for name in ('ids','isub','isubld','idsibpc')]
                vdi=inner[i] if family==2 else vd
                assert vdi>=0.,(family,polarity,temp,config,i,vdi)
                assert all(map(math.isfinite,values)),values
                f.write(' '.join(map(str,(family,polarity,temp,length,width,m,nf,config,vg,vd,vb,vdi,*values)))+'\n')

if __name__=='__main__':
    main()
