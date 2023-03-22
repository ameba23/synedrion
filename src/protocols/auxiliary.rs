use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crypto_bigint::Pow;
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use super::common::{SchemeParams, SessionId};
use super::generic::{BroadcastRound, NeedsConsensus, Round, ToSendTyped};
use crate::paillier::{
    encryption::Ciphertext,
    keys::{PublicKeyPaillier, SecretKeyPaillier},
    params::PaillierParams,
    uint::{Retrieve, UintLike},
};
use crate::sigma::fac::FacProof;
use crate::sigma::mod_::ModProof;
use crate::sigma::prm::PrmProof;
use crate::sigma::sch::{SchCommitment, SchProof, SchSecret};
use crate::tools::collections::{HoleVec, PartyIdx};
use crate::tools::group::{zero_sum_scalars, NonZeroScalar, Point, Scalar};
use crate::tools::hashing::{Chain, Hash};
use crate::tools::random::random_bits;

pub struct Round1<P: SchemeParams> {
    data: FullData<P>,
    secret_data: SecretData<P>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullData<P: SchemeParams> {
    session_id: SessionId,                                        // $sid$
    party_idx: PartyIdx,                                          // $i$
    xs_public: Vec<Point>,                                        // $\bm{X}_i$
    sch_commitments_x: Vec<SchCommitment>,                        // $\bm{A}_i$
    y_public: Point,                                              // $Y_i$,
    sch_commitment_y: SchCommitment,                              // $B_i$
    paillier_pk: PublicKeyPaillier<P::Paillier>,                  // $N_i$
    paillier_public: <P::Paillier as PaillierParams>::DoubleUint, // $s_i$
    paillier_base: <P::Paillier as PaillierParams>::DoubleUint,   // $t_i$
    prm_proof: PrmProof<P::Paillier>,                             // $\hat{\psi}_i$
    rho_bits: Box<[u8]>,                                          // $\rho_i$
    u_bits: Box<[u8]>,                                            // $u_i$
}

struct SecretData<P: SchemeParams> {
    paillier_sk: SecretKeyPaillier<P::Paillier>,
    y_secret: NonZeroScalar,
    xs_secret: Vec<Scalar>,
    sch_secret_y: SchSecret,
    sch_secrets_x: Vec<SchSecret>,
}

impl<P: SchemeParams> FullData<P> {
    fn hash(&self) -> Box<[u8]> {
        Hash::new_with_dst(b"Auxiliary")
            .chain(&self.session_id)
            .chain(&self.party_idx)
            .chain(&self.xs_public)
            .chain(&self.sch_commitments_x)
            .chain(&self.y_public)
            .chain(&self.sch_commitment_y)
            .chain(&self.paillier_pk)
            .chain(&self.paillier_public)
            .chain(&self.paillier_base)
            .chain(&self.prm_proof)
            .chain(&self.rho_bits)
            .chain(&self.u_bits)
            .finalize_boxed()
    }
}

impl<P: SchemeParams> Round1<P> {
    pub fn new(
        rng: &mut (impl RngCore + CryptoRng),
        session_id: &SessionId,
        party_idx: PartyIdx,
        num_parties: usize,
    ) -> Self {
        let paillier_sk = SecretKeyPaillier::<P::Paillier>::random(rng);
        let paillier_pk = paillier_sk.public_key();
        let y_secret = NonZeroScalar::random(rng);
        let y_public = &Point::GENERATOR * &y_secret;

        let sch_secret_y = SchSecret::random(rng); // $\tau$
        let sch_commitment_y = SchCommitment::new(&sch_secret_y); // $B_i$

        let xs_secret = zero_sum_scalars(rng, num_parties);
        let xs_public = xs_secret
            .iter()
            .cloned()
            .map(|x| &Point::GENERATOR * &x)
            .collect();

        let r = paillier_pk.random_invertible_group_elem(rng);
        let lambda = paillier_sk.random_field_elem(rng);
        let paillier_base = r * &r; // TODO: use `square()` when it's available
        let paillier_public = paillier_base.pow(&lambda);

        let aux = (session_id, &party_idx);
        let prm_proof = PrmProof::random(
            rng,
            &paillier_sk,
            &lambda,
            &paillier_base,
            &paillier_public,
            &aux,
            P::SECURITY_PARAMETER,
        );

        // $\tau_j$
        let sch_secrets_x: Vec<SchSecret> =
            (0..num_parties).map(|_| SchSecret::random(rng)).collect();

        // $A_i^j$
        let sch_commitments_x = sch_secrets_x.iter().map(SchCommitment::new).collect();

        let rho_bits = random_bits(P::SECURITY_PARAMETER);
        let u_bits = random_bits(P::SECURITY_PARAMETER);

        let data = FullData {
            session_id: session_id.clone(),
            party_idx,
            xs_public,
            sch_commitments_x,
            y_public,
            sch_commitment_y,
            paillier_pk,
            paillier_public: paillier_public.retrieve(),
            paillier_base: paillier_base.retrieve(),
            prm_proof,
            rho_bits,
            u_bits,
        };

        let secret_data = SecretData {
            paillier_sk,
            y_secret,
            xs_secret,
            sch_secrets_x,
            sch_secret_y,
        };

        Self { data, secret_data }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Round1Bcast {
    hash: Box<[u8]>, // `V_j`
}

impl<P: SchemeParams> Round for Round1<P> {
    type Error = String;
    type Payload = Box<[u8]>;
    type Message = Round1Bcast;
    type NextRound = Round2<P>;

    fn to_send(&self, _rng: &mut (impl RngCore + CryptoRng)) -> ToSendTyped<Self::Message> {
        ToSendTyped::Broadcast(Round1Bcast {
            hash: self.data.hash(),
        })
    }

    fn verify_received(
        &self,
        _from: PartyIdx,
        msg: Self::Message,
    ) -> Result<Self::Payload, Self::Error> {
        Ok(msg.hash)
    }

    fn finalize(self, payloads: HoleVec<Self::Payload>) -> Self::NextRound {
        Round2 {
            data: self.data,
            secret_data: self.secret_data,
            hashes: payloads,
        }
    }
}

impl<P: SchemeParams> BroadcastRound for Round1<P> {}

impl<P: SchemeParams> NeedsConsensus for Round1<P> {}

pub struct Round2<P: SchemeParams> {
    data: FullData<P>,
    secret_data: SecretData<P>,
    hashes: HoleVec<Box<[u8]>>, // V_j
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(bound = "FullData<P>: Serialize")]
pub struct Round2Bcast<P: SchemeParams> {
    data: FullData<P>,
}

impl<P: SchemeParams> Round for Round2<P> {
    type Error = String;
    type Payload = FullData<P>;
    type Message = Round2Bcast<P>;
    type NextRound = Round3<P>;

    fn to_send(&self, _rng: &mut (impl RngCore + CryptoRng)) -> ToSendTyped<Self::Message> {
        ToSendTyped::Broadcast(Round2Bcast {
            data: self.data.clone(),
        })
    }

    fn verify_received(
        &self,
        from: PartyIdx,
        msg: Self::Message,
    ) -> Result<Self::Payload, Self::Error> {
        if &msg.data.hash() != self.hashes.get(from).unwrap() {
            return Err("Invalid hash".to_string());
        }

        if msg.data.paillier_pk.modulus().as_ref().bits() < 8 * P::SECURITY_PARAMETER {
            return Err("Paillier modulus is too small".to_string());
        }

        let sum_x: Point = msg.data.xs_public.iter().sum();
        if sum_x != Point::IDENTITY {
            return Err("Sum of X points is not identity".to_string());
        }

        let aux = (&self.data.session_id, &from);
        if !msg.data.prm_proof.verify(
            &msg.data.paillier_pk,
            &msg.data.paillier_base,
            &msg.data.paillier_public,
            &aux,
        ) {
            return Err("PRM verification failed".to_string());
        }

        Ok(msg.data)
    }

    fn finalize(self, payloads: HoleVec<Self::Payload>) -> Self::NextRound {
        // XOR the vectors together
        // TODO: is there a better way?
        let mut rho = self.data.rho_bits.clone();
        for data in payloads.iter() {
            for (i, x) in data.rho_bits.iter().enumerate() {
                rho[i] ^= x;
            }
        }

        Round3 {
            rho,
            data: self.data,
            secret_data: self.secret_data,
            datas: payloads,
        }
    }
}

impl<P: SchemeParams> BroadcastRound for Round2<P> {}

pub struct Round3<P: SchemeParams> {
    rho: Box<[u8]>,
    data: FullData<P>,
    secret_data: SecretData<P>,
    datas: HoleVec<FullData<P>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "ModProof<P::Paillier>: Serialize,
    FacProof<P::Paillier>: Serialize,
    Ciphertext<P::Paillier>: Serialize"))]
#[serde(bound(deserialize = "ModProof<P::Paillier>: for<'x> Deserialize<'x>,
    FacProof<P::Paillier>: for<'x> Deserialize<'x>,
    Ciphertext<P::Paillier>: for<'x> Deserialize<'x>"))]
pub struct FullData2<P: SchemeParams> {
    mod_proof: ModProof<P::Paillier>,        // `psi_j`
    fac_proof: FacProof<P::Paillier>,        // `phi_j,i`
    sch_proof_y: SchProof,                   // `pi_i`
    paillier_enc_x: Ciphertext<P::Paillier>, // `C_j,i`
    sch_proof_x: SchProof,                   // `psi_i,j`
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(bound = "FullData2<P>: Serialize + for<'x> Deserialize<'x>")]
pub struct Round3Direct<P: SchemeParams> {
    data2: FullData2<P>,
}

impl<P: SchemeParams> Round for Round3<P> {
    type Error = String;
    type Payload = Scalar;
    type Message = Round3Direct<P>;
    type NextRound = AuxData<P>;

    fn to_send(&self, rng: &mut (impl RngCore + CryptoRng)) -> ToSendTyped<Self::Message> {
        let aux = (&self.data.session_id, &self.rho, &self.data.party_idx);
        let mod_proof = ModProof::random(
            rng,
            &self.secret_data.paillier_sk,
            &aux,
            P::SECURITY_PARAMETER,
        );

        let sch_proof_y = SchProof::new(
            &self.secret_data.sch_secret_y,
            &self.secret_data.y_secret.clone().into_scalar(),
            &self.data.sch_commitment_y,
            &self.data.y_public,
            &aux,
        );

        let mut dms = Vec::new();
        for (party_idx, data) in self.datas.enumerate() {
            let fac_proof = FacProof::random(rng, &self.secret_data.paillier_sk, &aux);

            let x_secret = self.secret_data.xs_secret[party_idx.as_usize()];
            let x_public = self.data.xs_public[party_idx.as_usize()];
            let ciphertext = Ciphertext::new(rng, &data.paillier_pk, &x_secret);

            let sch_proof_x = SchProof::new(
                &self.secret_data.sch_secrets_x[party_idx.as_usize()],
                &x_secret,
                &self.data.sch_commitments_x[party_idx.as_usize()],
                &x_public,
                &aux,
            );

            let data2 = FullData2 {
                mod_proof: mod_proof.clone(),
                fac_proof,
                sch_proof_y: sch_proof_y.clone(),
                paillier_enc_x: ciphertext,
                sch_proof_x,
            };

            dms.push((party_idx, Round3Direct { data2 }));
        }

        ToSendTyped::Direct(dms)
    }

    fn verify_received(
        &self,
        from: PartyIdx,
        msg: Self::Message,
    ) -> Result<Self::Payload, Self::Error> {
        let sender_data = &self.datas.get(from).unwrap();

        let x_secret = msg
            .data2
            .paillier_enc_x
            .decrypt(&self.secret_data.paillier_sk)
            .unwrap();

        if &Point::GENERATOR * &x_secret != sender_data.xs_public[self.data.party_idx.as_usize()] {
            // TODO: paper has `\mu` calculation here.
            return Err("Mismatched secret x".to_string());
        }

        let aux = (&self.data.session_id, &self.rho, &from);

        if !msg.data2.mod_proof.verify(&sender_data.paillier_pk, &aux) {
            return Err("Mod proof verification failed".to_string());
        }

        if !msg.data2.fac_proof.verify() {
            return Err("Fac proof verification failed".to_string());
        }

        if !msg
            .data2
            .sch_proof_y
            .verify(&sender_data.sch_commitment_y, &sender_data.y_public, &aux)
        {
            // CHECK: not sending the commitment the second time in `msg`,
            // since we already got it from the previous round.
            return Err("Sch proof verification (Y) failed".to_string());
        }

        if !msg.data2.sch_proof_x.verify(
            &sender_data.sch_commitments_x[self.data.party_idx.as_usize()],
            &sender_data.xs_public[self.data.party_idx.as_usize()],
            &aux,
        ) {
            // CHECK: not sending the commitment the second time in `msg`,
            // since we already got it from the previous round.
            return Err("Sch proof verification (Y) failed".to_string());
        }

        Ok(x_secret)
    }

    fn finalize(self, payloads: HoleVec<Self::Payload>) -> Self::NextRound {
        let secrets = payloads.into_vec(self.secret_data.xs_secret[self.data.party_idx.as_usize()]);
        let x_mask = secrets.iter().sum();

        let datas = self.datas.into_vec(self.data);

        let xs_masks_public = (0..datas.len())
            .map(|idx| datas.iter().map(|data| data.xs_public[idx]).sum())
            .collect::<Vec<_>>();

        let ys_public = datas.iter().map(|data| data.y_public).collect::<Vec<_>>();

        let paillier_pks = datas
            .iter()
            .map(|data| data.paillier_pk.clone())
            .collect::<Vec<_>>();

        let paillier_bases = datas
            .iter()
            .map(|data| data.paillier_base)
            .collect::<Vec<_>>();

        let paillier_publics = datas
            .iter()
            .map(|data| data.paillier_public)
            .collect::<Vec<_>>();

        AuxData {
            x_mask,
            y: self.secret_data.y_secret.clone(),
            paillier_sk: self.secret_data.paillier_sk,
            xs_masks_public,
            ys_public,
            paillier_pks,
            paillier_bases,
            paillier_publics,
        }
    }
}

pub struct AuxData<P: SchemeParams> {
    // secret
    x_mask: Scalar,
    y: NonZeroScalar,
    paillier_sk: SecretKeyPaillier<P::Paillier>,

    // public
    xs_masks_public: Vec<Point>,
    ys_public: Vec<Point>,
    paillier_pks: Vec<PublicKeyPaillier<P::Paillier>>,
    paillier_bases: Vec<<P::Paillier as PaillierParams>::DoubleUint>,
    paillier_publics: Vec<<P::Paillier as PaillierParams>::DoubleUint>,
}

#[cfg(test)]
mod tests {

    use rand_core::OsRng;

    use super::Round1;
    use crate::protocols::common::{SessionId, TestSchemeParams};
    use crate::protocols::generic::tests::step;
    use crate::tools::collections::PartyIdx;

    #[test]
    fn execute_auxiliary() {
        let session_id = SessionId::random();

        let r1 = vec![
            Round1::<TestSchemeParams>::new(&mut OsRng, &session_id, PartyIdx::from_usize(0), 3),
            Round1::<TestSchemeParams>::new(&mut OsRng, &session_id, PartyIdx::from_usize(1), 3),
            Round1::<TestSchemeParams>::new(&mut OsRng, &session_id, PartyIdx::from_usize(2), 3),
        ];

        let r2 = step(&mut OsRng, r1).unwrap();
        let r3 = step(&mut OsRng, r2).unwrap();
        let _aux_datas = step(&mut OsRng, r3).unwrap();

        // TODO: do some checks here
    }
}
